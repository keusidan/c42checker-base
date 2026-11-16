use crate::args::normalize_args;
use crate::compdb::ensure_generated;
use crate::config::{build_context, parse_settings_value, TOGGLES};
use crate::pipeline::{run_pipeline, PipelineHooks, PipelineRequest};
use crate::proto::sync_prototypes;
use crate::steps::{run_step_by_id, selected_steps_from_settings};
use crate::tools::detect_tools;
use crate::types::{Context, Diag, Settings, Severity, StepId, StepResult};
use crate::workdir::WORK_DIR_NAME;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

struct ServerState {
    root: PathBuf,
    settings: Settings,
    dirty_files: HashSet<PathBuf>,
    current_cancel: Option<Arc<AtomicBool>>,
    published_files: HashSet<String>,
}

impl ServerState {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            settings: Settings::default(),
            dirty_files: HashSet::new(),
            current_cancel: None,
            published_files: HashSet::new(),
        }
    }

    fn make_context(&self, cancel: Arc<AtomicBool>) -> Context {
        let dirty: Vec<PathBuf> = self.dirty_files.iter().cloned().collect();
        build_context(self.root.clone(), self.settings.clone(), cancel, dirty)
    }
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut decoded = String::with_capacity(rest.len());
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    decoded.push(b as char);
                    i += 3;
                    continue;
                }
            }
        }
        decoded.push(bytes[i] as char);
        i += 1;
    }
    Some(PathBuf::from(decoded))
}

fn path_to_uri(path: &str) -> String {
    format!("file://{path}")
}

fn send_message(out: &Arc<Mutex<io::Stdout>>, msg: &Value) {
    let body = serde_json::to_string(msg).unwrap_or_default();
    let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    if let Ok(mut guard) = out.lock() {
        let _ = guard.write_all(frame.as_bytes());
        let _ = guard.flush();
    }
}

fn send_response(out: &Arc<Mutex<io::Stdout>>, id: &Value, result: Value) {
    send_message(
        out,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }),
    );
}

fn send_error(out: &Arc<Mutex<io::Stdout>>, id: &Value, code: i32, message: &str) {
    send_message(
        out,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        }),
    );
}

fn send_notification(out: &Arc<Mutex<io::Stdout>>, method: &str, params: Value) {
    send_message(
        out,
        &json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }),
    );
}

fn publish_diags(
    out: &Arc<Mutex<io::Stdout>>,
    state: &Arc<Mutex<ServerState>>,
    diags: &[Diag],
) {
    let mut by_file: HashMap<String, Vec<Value>> = HashMap::new();
    for d in diags {
        let ln = d.line.saturating_sub(1);
        let col = d.col.saturating_sub(1);
        let sev = match d.severity {
            Severity::Error => 1,
            Severity::Warning => 2,
            Severity::Info => 3,
        };
        by_file.entry(d.file.clone()).or_default().push(json!({
            "range": {
                "start": { "line": ln, "character": col },
                "end": { "line": ln, "character": col + 1 },
            },
            "severity": sev,
            "source": format!("c42checker ({})", d.source),
            "message": d.message,
        }));
    }

    let prev_files: Vec<String> = {
        let mut st = state.lock().unwrap();
        let prev: Vec<String> = st.published_files.iter().cloned().collect();
        st.published_files = by_file.keys().cloned().collect();
        prev
    };

    for old in prev_files {
        if !by_file.contains_key(&old) {
            send_notification(
                out,
                "textDocument/publishDiagnostics",
                json!({ "uri": path_to_uri(&old), "diagnostics": [] }),
            );
        }
    }

    for (file, list) in by_file {
        send_notification(
            out,
            "textDocument/publishDiagnostics",
            json!({ "uri": path_to_uri(&file), "diagnostics": list }),
        );
    }
}

struct LspHooks {
    out: Arc<Mutex<io::Stdout>>,
}

impl PipelineHooks for LspHooks {
    fn on_stage(&mut self, stage: u8, label: &str) {
        send_notification(
            &self.out,
            "$/c42checker/stage",
            json!({ "stage": stage, "label": label }),
        );
    }

    fn on_step_start(&mut self, id: &str, label: &str) {
        send_notification(
            &self.out,
            "$/c42checker/stepStart",
            json!({ "id": id, "label": label }),
        );
    }

    fn on_step_end(&mut self, result: &StepResult) {
        send_notification(
            &self.out,
            "$/c42checker/stepResult",
            serde_json::to_value(result).unwrap_or(Value::Null),
        );
    }
}

fn read_lsp_message<R: BufRead>(reader: &mut R) -> io::Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            if k.eq_ignore_ascii_case("Content-Length") {
                content_length = v.trim().parse::<usize>().ok();
            }
        }
    }
    let Some(len) = content_length else {
        return Ok(None);
    };
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    let val: Value = serde_json::from_slice(&buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(val))
}

pub fn run_lsp_server() {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let out = Arc::new(Mutex::new(io::stdout()));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let state = Arc::new(Mutex::new(ServerState::new(cwd)));
    let mut shutdown_requested = false;

    while let Ok(Some(msg)) = read_lsp_message(&mut reader) {
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                if let Some(root_uri) = params.get("rootUri").and_then(|v| v.as_str()) {
                    if let Some(p) = uri_to_path(root_uri) {
                        state.lock().unwrap().root = p;
                    }
                } else if let Some(root_path) = params.get("rootPath").and_then(|v| v.as_str()) {
                    state.lock().unwrap().root = PathBuf::from(root_path);
                }
                if let Some(init_opts) = params.get("initializationOptions") {
                    if !init_opts.is_null() {
                        state.lock().unwrap().settings = parse_settings_value(init_opts);
                    }
                }
                if let Some(ref req_id) = id {
                    send_response(
                        &out,
                        req_id,
                        json!({
                            "capabilities": {
                                "textDocumentSync": {
                                    "openClose": true,
                                    "change": 1,
                                    "save": { "includeText": false }
                                },
                                "codeActionProvider": true,
                                "executeCommandProvider": {
                                    "commands": [
                                        "c42checker.run",
                                        "c42checker.runStep",
                                        "c42checker.cancel",
                                        "c42checker.detectTools",
                                        "c42checker.syncPrototypes",
                                        "c42checker.regenerateCompdb",
                                        "c42checker.cleanWorkDir",
                                        "c42checker.getContext"
                                    ]
                                }
                            },
                            "serverInfo": {
                                "name": "c42checker-lsp",
                                "version": env!("CARGO_PKG_VERSION")
                            }
                        }),
                    );
                }
            }
            "initialized" => {}
            "shutdown" => {
                shutdown_requested = true;
                if let Some(ref req_id) = id {
                    send_response(&out, req_id, Value::Null);
                }
            }
            "exit" => {
                std::process::exit(if shutdown_requested { 0 } else { 1 });
            }
            "workspace/didChangeConfiguration" => {
                if let Some(settings_val) = params.get("settings") {
                    let parsed = parse_settings_value(settings_val);
                    state.lock().unwrap().settings = parsed;
                }
            }
            "textDocument/didOpen" => {
                if let Some(uri) = params.pointer("/textDocument/uri").and_then(|v| v.as_str()) {
                    if let Some(p) = uri_to_path(uri) {
                        state.lock().unwrap().dirty_files.remove(&p);
                    }
                }
            }
            "textDocument/didChange" => {
                if let Some(uri) = params.pointer("/textDocument/uri").and_then(|v| v.as_str()) {
                    if let Some(p) = uri_to_path(uri) {
                        state.lock().unwrap().dirty_files.insert(p);
                    }
                }
            }
            "textDocument/didClose" => {
                if let Some(uri) = params.pointer("/textDocument/uri").and_then(|v| v.as_str()) {
                    if let Some(p) = uri_to_path(uri) {
                        state.lock().unwrap().dirty_files.remove(&p);
                    }
                }
            }
            "textDocument/didSave" => {
                if let Some(uri) = params.pointer("/textDocument/uri").and_then(|v| v.as_str()) {
                    if let Some(p) = uri_to_path(uri) {
                        let should_sync = {
                            let mut st = state.lock().unwrap();
                            st.dirty_files.remove(&p);
                            st.settings.proto_sync_on_save
                                && p.extension().and_then(|e| e.to_str()) == Some("c")
                        };
                        if should_sync {
                            let ctx = state
                                .lock()
                                .unwrap()
                                .make_context(Arc::new(AtomicBool::new(false)));
                            let res = sync_prototypes(&ctx, None, true, false);
                            send_notification(
                                &out,
                                "$/c42checker/protoSynced",
                                serde_json::to_value(res).unwrap_or(Value::Null),
                            );
                        }
                    }
                }
            }
            "textDocument/codeAction" => {
                if let Some(ref req_id) = id {
                    let actions = json!([
                        {
                            "title": "c42checker: ヘッダにプロトタイプを反映",
                            "kind": "source.fixAll",
                            "command": {
                                "title": "c42checker: ヘッダにプロトタイプを反映",
                                "command": "c42checker.syncPrototypes",
                                "arguments": [{ "apply": true }]
                            }
                        }
                    ]);
                    send_response(&out, req_id, actions);
                }
            }
            "workspace/executeCommand" => {
                let Some(ref req_id) = id else {
                    continue;
                };
                let cmd = params.get("command").and_then(|v| v.as_str()).unwrap_or("");
                let args = params
                    .get("arguments")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let arg0 = args.first().cloned().unwrap_or(Value::Null);

                handle_command(cmd, arg0, req_id.clone(), Arc::clone(&state), Arc::clone(&out));
            }
            _ => {
                if let Some(ref req_id) = id {
                    send_error(&out, req_id, -32601, &format!("Method not found: {method}"));
                }
            }
        }
    }
}

fn handle_command(
    cmd: &str,
    arg0: Value,
    req_id: Value,
    state: Arc<Mutex<ServerState>>,
    out: Arc<Mutex<io::Stdout>>,
) {
    match cmd {
        "c42checker.cancel" => {
            if let Some(ref cancel) = state.lock().unwrap().current_cancel {
                cancel.store(true, Ordering::Relaxed);
            }
            send_response(&out, &req_id, json!({ "cancelled": true }));
        }
        "c42checker.detectTools" => {
            let tools = detect_tools();
            let settings = state.lock().unwrap().settings.clone();
            let steps: Vec<Value> = StepId::ALL
                .iter()
                .map(|id| {
                    json!({
                        "id": id.as_str(),
                        "label": id.label(),
                        "stage": id.stage(),
                        "checked": settings.checks.get(id).copied().unwrap_or(false),
                    })
                })
                .collect();
            send_response(
                &out,
                &req_id,
                json!({
                    "tools": tools,
                    "steps": steps,
                    "toggles": TOGGLES,
                    "settings": settings,
                }),
            );
        }
        "c42checker.getContext" => {
            let ctx = state
                .lock()
                .unwrap()
                .make_context(Arc::new(AtomicBool::new(false)));
            let debug_prog = ctx.work_dir.join("debug").join("prog");
            let run_args = normalize_args(&ctx.settings.run_args, &ctx.root);
            send_response(
                &out,
                &req_id,
                json!({
                    "root": ctx.root,
                    "targetRoot": ctx.target_root,
                    "workDir": ctx.work_dir,
                    "hasMain": ctx.has_main,
                    "mainFile": ctx.main_file,
                    "hasMakefile": ctx.has_makefile,
                    "sources": ctx.sources,
                    "headers": ctx.headers,
                    "includeDirs": ctx.include_dirs,
                    "tools": ctx.tools,
                    "debugProgram": debug_prog,
                    "runArgs": run_args,
                }),
            );
        }
        "c42checker.syncPrototypes" => {
            let header = arg0.get("header").and_then(|v| v.as_str()).map(String::from);
            let apply = arg0.get("apply").and_then(|v| v.as_bool()).unwrap_or(false);
            let insert_markers = arg0
                .get("insertMarkers")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let ctx = state
                .lock()
                .unwrap()
                .make_context(Arc::new(AtomicBool::new(false)));
            let res = sync_prototypes(&ctx, header.as_deref(), apply, insert_markers);
            send_response(&out, &req_id, serde_json::to_value(res).unwrap_or(Value::Null));
        }
        "c42checker.regenerateCompdb" => {
            let ctx = state
                .lock()
                .unwrap()
                .make_context(Arc::new(AtomicBool::new(false)));
            match ensure_generated(&ctx, true) {
                Ok(g) => send_response(
                    &out,
                    &req_id,
                    json!({
                        "ok": true,
                        "entries": g.result.as_ref().map(|r| r.entries).unwrap_or(0),
                        "clangdWritten": g.clangd_written,
                    }),
                ),
                Err(e) => send_response(&out, &req_id, json!({ "ok": false, "error": e })),
            }
        }
        "c42checker.cleanWorkDir" => {
            let root = state.lock().unwrap().root.clone();
            let wd = root.join(WORK_DIR_NAME);
            let res = if wd.exists() {
                fs::remove_dir_all(&wd).map_err(|e| e.to_string())
            } else {
                Ok(())
            };
            match res {
                Ok(()) => send_response(&out, &req_id, json!({ "ok": true })),
                Err(e) => send_response(&out, &req_id, json!({ "ok": false, "error": e })),
            }
        }
        "c42checker.runStep" => {
            let step_str = arg0.get("stepId").and_then(|v| v.as_str()).unwrap_or("");
            let Some(step_id) = StepId::from_str_id(step_str) else {
                send_error(&out, &req_id, -32602, &format!("Unknown stepId: {step_str}"));
                return;
            };
            let cancel = Arc::new(AtomicBool::new(false));
            let ctx = {
                let mut st = state.lock().unwrap();
                if let Some(ref prev) = st.current_cancel {
                    prev.store(true, Ordering::Relaxed);
                }
                st.current_cancel = Some(cancel.clone());
                st.make_context(cancel)
            };
            std::thread::spawn(move || {
                let t0 = std::time::Instant::now();
                let outcome = run_step_by_id(&ctx, step_id);
                let result = StepResult::from_outcome(
                    step_id.as_str(),
                    step_id.label(),
                    step_id.stage(),
                    t0.elapsed().as_millis() as u64,
                    outcome,
                );
                publish_diags(&out, &state, &result.diags);
                send_response(&out, &req_id, serde_json::to_value(&result).unwrap_or(Value::Null));
            });
        }
        "c42checker.run" => {
            let cancel = Arc::new(AtomicBool::new(false));
            let ctx = {
                let mut st = state.lock().unwrap();
                if let Some(ref prev) = st.current_cancel {
                    prev.store(true, Ordering::Relaxed);
                }
                st.current_cancel = Some(cancel.clone());
                st.make_context(cancel)
            };
            let selected = if let Some(arr) = arg0.get("selected").and_then(|v| v.as_array()) {
                arr.iter()
                    .filter_map(|v| v.as_str().and_then(StepId::from_str_id))
                    .collect()
            } else {
                selected_steps_from_settings(&ctx)
            };
            let debug_build = arg0
                .get("debugBuild")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let debug_build_only = arg0
                .get("debugBuildOnly")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            std::thread::spawn(move || {
                let mut hooks = LspHooks {
                    out: Arc::clone(&out),
                };
                let report = run_pipeline(
                    &ctx,
                    &PipelineRequest {
                        selected,
                        debug_build,
                        debug_build_only,
                    },
                    &mut hooks,
                );
                let all_diags: Vec<Diag> = report
                    .results
                    .iter()
                    .flat_map(|r| r.diags.iter().cloned())
                    .collect();
                publish_diags(&out, &state, &all_diags);
                send_response(
                    &out,
                    &req_id,
                    serde_json::to_value(&report).unwrap_or(Value::Null),
                );
            });
        }
        _ => {
            send_error(&out, &req_id, -32601, &format!("Unknown command: {cmd}"));
        }
    }
}
