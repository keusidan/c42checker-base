use c42checker_base::args::shell_split;
use c42checker_base::compdb::parse_make_dry_run;
use c42checker_base::config::parse_settings_value;
use c42checker_base::ctags::resolve_ctags_in_path;
use c42checker_base::exclude::matches_pattern;
use c42checker_base::makefile::check_makefile_text;
use c42checker_base::parse::{
    drop_norminette_errors, parse_compiler_output, parse_leaks, parse_norminette,
    NORMINETTE_PARAMETER_CODES,
};
use c42checker_base::proto::{normalize_params, normalize_return_type, render_block, ProtoEntry};
use c42checker_base::tools::{augment_darwin_path, detect_tools_for_os, find_all_on_path, find_highest_versioned};
use c42checker_base::types::FailFast;
use c42checker_base::workdir::{copy_project, parse_df_avail_mb};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

#[test]
fn test_shell_split_quotes_and_escapes() {
    assert_eq!(
        shell_split(r#"-DFOO="hello world" -I'a b' -lm"#),
        vec!["-DFOO=hello world", "-Ia b", "-lm"]
    );
}

#[test]
fn test_exclude_patterns() {
    assert!(matches_pattern("src/main.c", "main.c"));
    assert!(!matches_pattern("src/main.c", "/main.c"));
    assert!(matches_pattern("main.c", "/main.c"));
    assert!(matches_pattern("tests/foo/bar.c", "tests/"));
}

#[test]
fn test_parse_compiler_and_norminette() {
    let diags = parse_compiler_output(
        "src/foo.c:12:5: warning: unused variable 'x'\nsrc/bar.c:3:1: error: expected ';'\n",
        Path::new("/proj"),
        "clang",
    );
    assert_eq!(diags.len(), 2);
    assert_eq!(diags[0].file, "/proj/src/foo.c");
    assert_eq!(diags[0].line, 12);
    assert_eq!(diags[0].col, 5);

    let norm_out = "foo.c: Error!\nError: TOO_MANY_ARGS       (line:  10, col:   1):\tFunction has more than 4 arguments\n";
    let nd = parse_norminette(norm_out, Path::new("/proj"));
    assert_eq!(nd.len(), 1);
    assert_eq!(nd[0].file, "/proj/foo.c");

    let dropped = drop_norminette_errors(norm_out, NORMINETTE_PARAMETER_CODES);
    assert_eq!(dropped.dropped, 1);
    assert_eq!(dropped.remaining, 0);
    assert!(dropped.text.contains("foo.c: OK!"));
}

#[test]
fn test_proto_formatting() {
    let ret = normalize_return_type(Some("typename:char **")).unwrap();
    assert_eq!(ret.base, "char");
    assert_eq!(ret.stars, "**");
    let params = normalize_params(Some("(int n, const char * s)")).unwrap();
    assert_eq!(params, "(int n, const char *s)");

    let block = render_block(&[ProtoEntry {
        name: "ft_split".to_string(),
        ret,
        params,
    }]);
    assert_eq!(block[0], "char\t**ft_split(int n, const char *s);");
}

#[test]
fn test_makefile_static_checks() {
    let bad = "SRCS = $(wildcard *.c)\nall:\n\techo hi\n";
    let findings = check_makefile_text(bad, Path::new("."));
    assert!(findings.iter().any(|f| f.message.contains("NAME")));
    assert!(findings.iter().any(|f| f.message.contains("wildcard")));
}

#[test]
fn test_parse_make_dry_run() {
    let out = "cc -Wall -Wextra -Werror -Iincludes -c src/main.c -o src/main.o\n";
    let entries = parse_make_dry_run(out, Path::new("/proj"));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].file, "/proj/src/main.c");
    assert!(entries[0].arguments.contains(&"-Iincludes".to_string()));
}

#[test]
fn test_parse_settings_nested() {
    let val = json!({
        "c42checker": {
            "failFast": "step",
            "debug": { "sanitizer": true },
            "checks": { "tsan": true }
        }
    });
    let s = parse_settings_value(&val);
    assert_eq!(s.fail_fast, FailFast::Step);
    assert!(s.debug_sanitizer);
    assert_eq!(s.checks.get(&c42checker_base::types::StepId::Tsan), Some(&true));
}

fn send_lsp(stdin: &mut impl Write, msg: &Value) {
    let body = serde_json::to_string(msg).unwrap();
    let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    stdin.write_all(frame.as_bytes()).unwrap();
    stdin.flush().unwrap();
}

fn read_lsp(reader: &mut BufReader<impl Read>) -> Value {
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let t = line.trim();
        if t.is_empty() {
            break;
        }
        if let Some((k, v)) = t.split_once(':') {
            if k.eq_ignore_ascii_case("Content-Length") {
                len = v.trim().parse().unwrap();
            }
        }
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).unwrap();
    serde_json::from_slice(&buf).unwrap()
}

#[test]
fn test_lsp_initialize_and_detect_tools() {
    let bin = env!("CARGO_BIN_EXE_c42checker-lsp");
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    send_lsp(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "rootUri": "file:///tmp", "capabilities": {} }
        }),
    );
    let init_resp = read_lsp(&mut reader);
    assert_eq!(init_resp["id"], 1);
    assert!(init_resp["result"]["capabilities"]["executeCommandProvider"]["commands"].is_array());

    send_lsp(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    send_lsp(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "workspace/executeCommand",
            "params": { "command": "c42checker.detectTools", "arguments": [] }
        }),
    );
    let tools_resp = read_lsp(&mut reader);
    assert_eq!(tools_resp["id"], 2);
    assert!(tools_resp["result"]["steps"].as_array().unwrap().len() == 15);

    send_lsp(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": null }),
    );
    let shut_resp = read_lsp(&mut reader);
    assert_eq!(shut_resp["id"], 3);

    send_lsp(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
    );
    let status = child.wait().unwrap();
    assert!(status.success());
}

#[test]
fn test_macos_tool_detection_skips_apple_clang_gcc() {
    let base = std::env::temp_dir().join(format!("c42_tools_test_{}", std::process::id()));
    let sys_dir = base.join("sys");
    let brew_dir = base.join("brew");
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&sys_dir).unwrap();
    fs::create_dir_all(&brew_dir).unwrap();

    let mk = |dir: &Path, name: &str, body: &str| {
        let p = dir.join(name);
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = fs::metadata(&p).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&p, perms).unwrap();
        p.to_string_lossy().into_owned()
    };

    let apple_gcc = mk(&sys_dir, "gcc", "echo 'Apple clang version 15.0.0'");
    let gnu_gcc14 = mk(&brew_dir, "gcc-14", "echo 'gcc-14 (Homebrew GCC 14.2.0) 14.2.0'");

    let sys_str = sys_dir.to_string_lossy();
    let both_str = format!("{}:{}", sys_dir.display(), brew_dir.display());

    assert_eq!(
        find_highest_versioned("gcc", &both_str).as_deref(),
        Some(gnu_gcc14.as_str())
    );
    assert_eq!(detect_tools_for_os(&sys_str, true).gcc, None);
    assert_eq!(detect_tools_for_os(&sys_str, false).gcc, Some(apple_gcc));
    assert_eq!(detect_tools_for_os(&both_str, true).gcc, Some(gnu_gcc14));

    let path = augment_darwin_path("/usr/bin", Some("/Users/alice"));
    assert!(path.contains("/opt/homebrew/bin"));
    assert!(path.contains("/Users/alice/.local/bin"));
    assert!(path.contains("/Users/alice/.cargo/bin"));

    // BSD /usr/bin/ctags の後ろにある Homebrew universal-ctags を検出できること
    let _bsd_ctags = mk(&sys_dir, "ctags", "echo 'ctags: illegal option' >&2; exit 1");
    let brew_ctags = mk(&brew_dir, "ctags", "echo 'Universal Ctags 6.1.0'");
    assert_eq!(find_all_on_path("ctags", &both_str).len(), 2);
    assert_eq!(
        resolve_ctags_in_path(&both_str).bin.as_deref(),
        Some(brew_ctags.as_str())
    );

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn test_macos_leaks_parser_and_workdir() {
    let base = std::env::temp_dir().join(format!("c42_leaks_test_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(base.join("prog.dSYM/Contents")).unwrap();
    let main_c = base.join("main.c");
    fs::write(&main_c, "int main(void) { return 0; }\n").unwrap();
    fs::write(base.join(".DS_Store"), "junk").unwrap();
    fs::write(base.join("libfoo.dylib"), "junk").unwrap();
    fs::write(base.join("prog.dSYM/Contents/Info.plist"), "junk").unwrap();

    let out = format!(
        "Process 99: 1 leak for 16 total leaked bytes.\n\
         STACK OF 1 INSTANCE OF 'ROOT LEAK: <malloc in main>':\n\
         2   prog   0x100003f50 main + 48  {}:12\n\
         ====\n",
        main_c.display()
    );
    let parsed = parse_leaks(&out, &base);
    assert_eq!(parsed.leak_count, 1);
    assert_eq!(parsed.leaked_bytes, 16);
    assert_eq!(parsed.diags.len(), 1);
    assert_eq!(parsed.diags[0].line, 12);

    let list_out = "Process 99: 1 leak for 32 total leaked bytes.\n\
         Leak: 0x600000004010  size=32  zone: DefaultMallocZone\n\
             Call stack: [thread 0x1]: | start | main (in prog) + 48  main.c:20 | malloc\n";
    let parsed_list = parse_leaks(list_out, &base);
    assert_eq!(parsed_list.leak_count, 1);
    assert_eq!(parsed_list.leaked_bytes, 32);
    assert_eq!(parsed_list.diags.len(), 1);
    assert_eq!(parsed_list.diags[0].line, 20);

    // copy_project が macOS 成果物を除外すること
    let dst = std::env::temp_dir().join(format!("c42_leaks_dst_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dst);
    copy_project(&base, &dst, 10 * 1024 * 1024).unwrap();
    assert!(dst.join("main.c").is_file());
    assert!(!dst.join(".DS_Store").exists());
    assert!(!dst.join("libfoo.dylib").exists());
    assert!(!dst.join("prog.dSYM").exists());

    // macOS df -Pm で Filesystem / Mounted on に空白があっても Available を取れること
    let mac_df = "Filesystem 1048576-blocks Used Available Capacity Mounted on\nmap auto_home 100000 40000 60000 40% /Volumes/Macintosh HD\n";
    assert_eq!(parse_df_avail_mb(mac_df).unwrap(), 60000);

    let _ = fs::remove_dir_all(&base);
    let _ = fs::remove_dir_all(&dst);
}

#[cfg(target_os = "macos")]
#[test]
fn test_real_macos_tools_and_leaks_step() {
    use c42checker_base::config::build_context;
    use c42checker_base::ctags::resolve_ctags;
    use c42checker_base::steps::run_step_by_id;
    use c42checker_base::tools::{detect_macos_sdk_root, detect_tools, is_apple_clang};
    use c42checker_base::types::{Settings, StepId, StepStatus};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    let tools = detect_tools();
    assert!(tools.cc.is_some(), "macOS clang should be detected");
    assert!(tools.leaks.is_some(), "macOS leaks should be detected");
    if let Some(ref gcc) = tools.gcc {
        assert!(!is_apple_clang(gcc), "detected gcc must not be Apple clang: {gcc}");
    }
    assert!(detect_macos_sdk_root().is_some(), "macOS SDKROOT should be detected");

    let ct = resolve_ctags();
    if let Some(bin) = ct.bin {
        assert_ne!(bin, "/usr/bin/ctags", "should skip BSD /usr/bin/ctags");
    }

    let proj = std::env::temp_dir().join(format!("c42_real_mac_{}", std::process::id()));
    let _ = fs::remove_dir_all(&proj);
    fs::create_dir_all(&proj).unwrap();
    fs::write(
        proj.join("main.c"),
        "#include <stdlib.h>\nstatic void leak_mem(void) { char *p = malloc(32); if (p) p[0] = 1; }\nint main(void) { leak_mem(); return 0; }\n",
    )
    .unwrap();
    let ctx = build_context(proj.clone(), Settings::default(), Arc::new(AtomicBool::new(false)), Vec::new());

    // warnings / clangTidy / msan / leaks on real macOS
    let w = run_step_by_id(&ctx, StepId::Warnings);
    assert_eq!(w.status, StepStatus::Pass, "warnings failed:\n{}", w.log);

    if ctx.tools.tidy.is_some() {
        let t = run_step_by_id(&ctx, StepId::ClangTidy);
        assert_ne!(
            t.log.contains("'stdlib.h' file not found"),
            true,
            "clang-tidy should find macOS SDK headers:\n{}",
            t.log
        );
    }

    let ms = run_step_by_id(&ctx, StepId::Msan);
    assert_eq!(ms.status, StepStatus::Skip, "MSan should skip on macOS:\n{}", ms.log);

    let lk = run_step_by_id(&ctx, StepId::Leaks);
    assert!(
        lk.status == StepStatus::Fail || lk.status == StepStatus::Skip,
        "leaks should fail on leak (or skip if SIP restricts):\n{}",
        lk.log
    );

    let _ = fs::remove_dir_all(&proj);
}
