use crate::exec::{exec_cmd, ExecOptions};
use crate::sources::find_makefile;
use crate::types::{Context, Diag, Severity};
use crate::workdir::copy_project;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct MakefileFinding {
    pub line: u32,
    pub is_error: bool,
    pub message: String,
}

pub struct LogicalLine {
    pub line: u32,
    pub text: String,
    pub recipe: bool,
}

pub fn logical_lines(text: &str) -> Vec<LogicalLine> {
    let raw: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let mut cur = raw[i].trim_end_matches('\r').to_string();
        let start = (i + 1) as u32;
        while cur.ends_with('\\') && i + 1 < raw.len() {
            cur.pop();
            i += 1;
            cur.push(' ');
            cur.push_str(raw[i].trim_end_matches('\r'));
        }
        let recipe = cur.starts_with('\t');
        let cleaned = strip_makefile_comment(&cur);
        if !cleaned.trim().is_empty() {
            out.push(LogicalLine {
                line: start,
                text: cleaned,
                recipe,
            });
        }
        i += 1;
    }
    out
}

fn strip_makefile_comment(s: &str) -> String {
    let bytes = s.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'#' {
            if i > 0 && (bytes[i - 1] == b'\\' || bytes[i - 1] == b'$') {
                continue;
            }
            return s[..i].to_string();
        }
    }
    s.to_string()
}

pub fn rule_targets(lines: &[LogicalLine]) -> Vec<String> {
    let mut targets = Vec::new();
    for l in lines {
        if l.recipe {
            continue;
        }
        let trimmed = l.text.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(colon) = l.text.find(':') {
            let lhs = &l.text[..colon];
            let rhs = &l.text[colon + 1..];
            if rhs.starts_with('=') || lhs.contains('=') {
                continue;
            }
            for t in lhs.split_whitespace() {
                if !t.is_empty() {
                    targets.push(t.to_string());
                }
            }
        }
    }
    targets
}

fn norm_var(t: &str) -> String {
    if t.starts_with("${") && t.ends_with('}') && t.len() > 3 {
        format!("$({})", &t[2..t.len() - 1])
    } else {
        t.to_string()
    }
}

pub fn check_makefile_text(text: &str, makefile_dir: &Path) -> Vec<MakefileFinding> {
    let lines = logical_lines(text);
    let mut out = Vec::new();
    let targets: Vec<String> = rule_targets(&lines).into_iter().map(|t| norm_var(&t)).collect();

    let name_defined = lines.iter().any(|l| {
        !l.recipe
            && l.text
                .trim_start()
                .strip_prefix("NAME")
                .is_some_and(|r| r.trim_start().starts_with(['=', ':', '?', '+']))
    });
    if !name_defined {
        out.push(MakefileFinding {
            line: 1,
            is_error: true,
            message: "変数 NAME が定義されていません。`$(NAME)` の規則は必須です (Norm III.11)".to_string(),
        });
    }
    for req in ["$(NAME)", "all", "clean", "fclean", "re"] {
        if !targets.iter().any(|t| t == req) {
            out.push(MakefileFinding {
                line: 1,
                is_error: true,
                message: format!("規則 {req} がありません。$(NAME)、all、clean、fclean、re の規則は必須です (Norm III.11)"),
            });
        }
    }

    for l in &lines {
        let is_removal = l.text.contains("rm ") || l.text.contains("rm\t") || l.text.contains("-delete");
        if l.text.contains("wildcard")
            || l.text.contains("$(shell find")
            || l.text.contains("$(shell ls")
            || ((l.text.contains("*.c") || l.text.contains("*.o")) && !is_removal)
        {
            out.push(MakefileFinding {
                line: l.line,
                is_error: true,
                message: "ソースファイルを wildcard や `*.c` で指定しています。コンパイルに必要な全てのソースは、Makefile に明示的に書きます (Norm III.11)".to_string(),
            });
        }
    }

    // Check bundled libraries referenced via -L
    let joined: String = lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
    let mut lib_dirs = HashSet::new();
    for tok in joined.split_whitespace() {
        if let Some(d) = tok.strip_prefix("-L") {
            if !d.is_empty() && !d.contains("$(") && !d.contains("${") {
                lib_dirs.insert(d.to_string());
            }
        }
    }
    for dir in lib_dirs {
        let abs = makefile_dir.join(&dir);
        if find_makefile(&abs).is_some() {
            let clean_d = dir.trim_start_matches("./").trim_end_matches('/');
            let built = joined.contains(&format!("-C {clean_d}"))
                || joined.contains(&format!("-C ./{clean_d}"))
                || joined.contains(&format!("cd {clean_d}"));
            if !built {
                out.push(MakefileFinding {
                    line: 1,
                    is_error: true,
                    message: format!("同梱のライブラリ {dir} (Makefile があります) を、この Makefile が自動でビルドしていません。`$(MAKE) -C {dir}` などでビルドさせてください (Norm III.11)"),
                });
            }
        }
    }

    out
}

fn snapshot_mtimes(dir: &Path) -> HashMap<String, SystemTime> {
    let mut map = HashMap::new();
    walk_snap(dir, dir, &mut map);
    map
}

fn walk_snap(root: &Path, dir: &Path, map: &mut HashMap<String, SystemTime>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let Ok(meta) = fs::metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            walk_snap(root, &p, map);
        } else if meta.is_file() {
            if let (Ok(rel), Ok(mt)) = (p.strip_prefix(root), meta.modified()) {
                map.insert(rel.to_string_lossy().into_owned(), mt);
            }
        }
    }
}

pub struct MakefileRun {
    pub findings: Vec<MakefileFinding>,
    pub log: String,
    pub note: Option<String>,
}

pub fn check_makefile(ctx: &Context, makefile: &Path) -> MakefileRun {
    let mk_dir = makefile.parent().unwrap_or(&ctx.target_root);
    let text = match fs::read_to_string(makefile) {
        Ok(t) => t,
        Err(e) => {
            return MakefileRun {
                findings: vec![MakefileFinding {
                    line: 1,
                    is_error: true,
                    message: format!("Makefile を読めません: {e}"),
                }],
                log: String::new(),
                note: None,
            };
        }
    };

    let mut findings = check_makefile_text(&text, mk_dir);
    let mut log = format!("Makefile: {}\n", makefile.display());

    let Some(ref make_bin) = ctx.tools.make else {
        return MakefileRun {
            findings,
            log,
            note: Some("make が見つからないため静的検査のみ行いました".to_string()),
        };
    };

    let tmp_copy = ctx.work_dir.join("makefile-check");
    let _ = fs::remove_dir_all(&tmp_copy);
    let max_bytes = ctx.settings.work_dir_max_mb * 1024 * 1024;
    if let Err(e) = copy_project(&ctx.target_root, &tmp_copy, max_bytes) {
        return MakefileRun {
            findings,
            log: format!("{log}{e}\n"),
            note: Some(e),
        };
    }

    let rel_dir = mk_dir.strip_prefix(&ctx.target_root).unwrap_or(Path::new(""));
    let work_cwd = tmp_copy.join(rel_dir);

    let run_make = |args: &[&str]| {
        let a: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        exec_cmd(
            make_bin,
            &a,
            &ExecOptions {
                cwd: &work_cwd,
                timeout_ms: ctx.settings.static_timeout_sec * 1000,
                cancelled: Some(ctx.cancelled.clone()),
                force_c_locale: true,
            },
        )
    };

    let r1 = run_make(&[]);
    log.push_str(&format!("$ make (code {:?})\n{}\n", r1.code, r1.output));
    if r1.code != Some(0) {
        findings.push(MakefileFinding {
            line: 1,
            is_error: true,
            message: format!("`make` が失敗しました (終了コード {:?})", r1.code),
        });
    } else {
        let snap1 = snapshot_mtimes(&tmp_copy);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let r2 = run_make(&[]);
        log.push_str(&format!("$ make (2回目, code {:?})\n{}\n", r2.code, r2.output));
        let snap2 = snapshot_mtimes(&tmp_copy);
        let relinked = snap2.iter().any(|(k, v)| snap1.get(k) != Some(v));
        if relinked || !r2.output.contains("Nothing to be done") && !r2.output.contains("is up to date") {
            if relinked {
                findings.push(MakefileFinding {
                    line: 1,
                    is_error: true,
                    message: "2 回目の `make` で再ビルド/relink が発生しています (Norm III.11)".to_string(),
                });
            }
        }
    }

    for target in ["clean", "fclean", "re"] {
        let r = run_make(&[target]);
        log.push_str(&format!("$ make {target} (code {:?})\n{}\n", r.code, r.output));
        if r.code != Some(0) {
            findings.push(MakefileFinding {
                line: 1,
                is_error: true,
                message: format!("`make {target}` が失敗しました (終了コード {:?})", r.code),
            });
        }
    }

    let _ = fs::remove_dir_all(&tmp_copy);
    MakefileRun {
        findings,
        log,
        note: None,
    }
}

pub fn to_diags(makefile: &Path, findings: &[MakefileFinding]) -> Vec<Diag> {
    let f = makefile.to_string_lossy().into_owned();
    findings
        .iter()
        .map(|x| Diag {
            file: f.clone(),
            line: x.line,
            col: 1,
            severity: if x.is_error {
                Severity::Error
            } else {
                Severity::Warning
            },
            message: x.message.clone(),
            source: "makefile".to_string(),
        })
        .collect()
}
