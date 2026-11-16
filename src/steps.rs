use crate::args::{extra_cflags, normalize_args, step_args};
use crate::build::{build_binary, build_inputs, NO_MAIN_HINT, NO_MAIN_REASON};
use crate::exec::{exec_cmd, ExecOptions};
use crate::exclude::is_excluded;
use crate::makefile::{check_makefile, to_diags as makefile_diags};
use crate::normextra::run_norm_extra;
use crate::parse::{
    drop_norminette_errors, parse_cbmc, parse_compiler_output, parse_frama_c, parse_norminette,
    parse_sanitizer, parse_valgrind, strip_ansi, NORMINETTE_PARAMETER_CODES,
};
use crate::sources::{find_makefile, make_target_exists};
use crate::tools::{install_hint, machine_arch, step_tool_name};
use crate::types::{Context, Diag, Severity, StepId, StepOutcome};
use crate::workdir::copy_project;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

fn skip_outcome(reason: impl Into<String>, hint: Option<&str>) -> StepOutcome {
    StepOutcome {
        status: crate::types::StepStatus::Skip,
        diags: Vec::new(),
        log: String::new(),
        reason: Some(reason.into()),
        hint: hint.map(String::from),
    }
}

fn need_tool(ctx: &Context, key: &str, label: &str) -> Option<StepOutcome> {
    let found = match key {
        "cc" => ctx.tools.cc.as_ref(),
        "tidy" => ctx.tools.tidy.as_ref(),
        "scanBuild" => ctx.tools.scan_build.as_ref(),
        "gcc" => ctx.tools.gcc.as_ref(),
        "valgrind" => ctx.tools.valgrind.as_ref(),
        "norminette" => ctx.tools.norminette.as_ref(),
        "make" => ctx.tools.make.as_ref(),
        "cbmc" => ctx.tools.cbmc.as_ref(),
        "framaC" => ctx.tools.frama_c.as_ref(),
        "cFormatter" => ctx.tools.c_formatter.as_ref(),
        _ => None,
    };
    if found.is_some() {
        None
    } else {
        Some(skip_outcome(
            format!("{label} が見つかりません"),
            Some(install_hint(key)),
        ))
    }
}

pub fn norm_targets(ctx: &Context) -> Vec<PathBuf> {
    ctx.sources
        .iter()
        .chain(ctx.headers.iter())
        .filter(|f| !is_excluded(f, &ctx.root, &ctx.settings.norm_exclude))
        .cloned()
        .collect()
}

fn verdict(
    code: Option<i32>,
    diags: Vec<Diag>,
    output: String,
    error: Option<String>,
    timed_out: bool,
) -> StepOutcome {
    if timed_out {
        return StepOutcome {
            status: crate::types::StepStatus::Fail,
            diags,
            log: output,
            reason: Some("制限時間 (c42checker.staticTimeoutSec) を超えました".to_string()),
            hint: None,
        };
    }
    if let Some(err) = error {
        return StepOutcome {
            status: crate::types::StepStatus::Fail,
            diags,
            log: output,
            reason: Some(err),
            hint: None,
        };
    }
    let err_count = diags.iter().filter(|d| d.severity == Severity::Error).count();
    let failed = code != Some(0) || err_count > 0;
    StepOutcome {
        status: if failed {
            crate::types::StepStatus::Fail
        } else {
            crate::types::StepStatus::Pass
        },
        diags,
        log: output,
        reason: if failed {
            Some(format!(
                "終了コード {:?}{}",
                code,
                if err_count > 0 {
                    format!("、error 診断 {err_count} 件")
                } else {
                    String::new()
                }
            ))
        } else {
            None
        },
        hint: None,
    }
}

fn inc_flags(ctx: &Context) -> Vec<String> {
    ctx.include_dirs
        .iter()
        .map(|d| format!("-I{}", d.display()))
        .collect()
}

pub fn run_step_by_id(ctx: &Context, id: StepId) -> StepOutcome {
    if let Some(tool_key) = step_tool_name(id) {
        if let Some(skip) = need_tool(ctx, tool_key, tool_key) {
            return skip;
        }
    }
    match id {
        StepId::CFormatter => run_c_formatter(ctx),
        StepId::Norminette => run_norminette(ctx),
        StepId::NormExtra => run_norm_extra_step(ctx),
        StepId::Makefile => run_makefile_step(ctx),
        StepId::Warnings => run_warnings(ctx),
        StepId::ClangTidy => run_clang_tidy(ctx),
        StepId::ScanBuild => run_scan_build(ctx),
        StepId::GccAnalyzer => run_gcc_analyzer(ctx),
        StepId::AsanUbsan => run_sanitizer_step(
            ctx,
            StepId::AsanUbsan,
            &["-g", "-O1", "-fno-omit-frame-pointer", "-fsanitize=address,undefined"],
            "asan",
            false,
        ),
        StepId::Tsan => run_sanitizer_step(
            ctx,
            StepId::Tsan,
            &["-g", "-O1", "-fno-omit-frame-pointer", "-fsanitize=thread", "-pthread"],
            "tsan",
            true,
        ),
        StepId::Msan => run_sanitizer_step(
            ctx,
            StepId::Msan,
            &["-g", "-O1", "-fno-omit-frame-pointer", "-fsanitize=memory", "-fsanitize-memory-track-origins"],
            "msan",
            true,
        ),
        StepId::Valgrind => run_valgrind_step(ctx),
        StepId::Cbmc => run_cbmc_step(ctx),
        StepId::FramaC => run_frama_c_step(ctx),
    }
}

fn run_c_formatter(ctx: &Context) -> StepOutcome {
    let files = norm_targets(ctx);
    if files.is_empty() {
        return skip_outcome(
            "対象のファイルがありません",
            Some("設定 `c42checker.normExclude` または `targetDir` を確認してください"),
        );
    }
    let dirty: Vec<&PathBuf> = files.iter().filter(|f| ctx.is_file_dirty(f)).collect();
    if !dirty.is_empty() {
        return skip_outcome(
            format!("未保存の変更があるファイルが {} 件あるため整形しませんでした", dirty.len()),
            Some("保存してから再実行してください"),
        );
    }

    let bin = ctx.tools.c_formatter.as_deref().unwrap();
    let before: Vec<(PathBuf, Vec<u8>)> = files
        .iter()
        .filter_map(|f| fs::read(f).ok().map(|b| (f.clone(), b)))
        .collect();

    let cwd = ctx.work_dir.join("format-cwd");
    let _ = fs::create_dir_all(&cwd);
    let mut args = step_args(ctx, StepId::CFormatter);
    for f in &files {
        args.push(f.to_string_lossy().into_owned());
    }
    let res = exec_cmd(
        bin,
        &args,
        &ExecOptions {
            cwd: &cwd,
            timeout_ms: ctx.settings.static_timeout_sec * 1000,
            cancelled: Some(ctx.cancelled.clone()),
            force_c_locale: true,
        },
    );
    let _ = fs::remove_dir_all(&cwd);

    let mut changed = Vec::new();
    let backup_dir = ctx.work_dir.join("format-backup");
    for (f, orig) in &before {
        if let Ok(now) = fs::read(f) {
            if &now != orig {
                changed.push((f.clone(), orig.clone()));
                let rel = f.strip_prefix(&ctx.root).unwrap_or(f);
                let bak = backup_dir.join(rel);
                if let Some(p) = bak.parent() {
                    let _ = fs::create_dir_all(p);
                }
                let _ = fs::write(&bak, orig);
            }
        }
    }

    if res.timed_out || res.error.is_some() || res.code != Some(0) {
        for (f, orig) in &changed {
            let _ = fs::write(f, orig);
        }
        return StepOutcome {
            status: crate::types::StepStatus::Fail,
            diags: Vec::new(),
            log: res.output,
            reason: Some(res.error.unwrap_or_else(|| format!("c_formatter_42 が終了コード {:?} で終了しました", res.code))),
            hint: None,
        };
    }

    let msg = if changed.is_empty() {
        "整形による変更はありません\n".to_string()
    } else {
        format!("整形で変更したファイル {} 件 (.c42checker/format-backup/ に退避)\n", changed.len())
    };
    StepOutcome {
        status: crate::types::StepStatus::Pass,
        diags: Vec::new(),
        log: format!("{}{}", res.output, msg),
        reason: if changed.is_empty() {
            None
        } else {
            Some(format!("{} ファイルを整形しました", changed.len()))
        },
        hint: None,
    }
}

fn run_norminette(ctx: &Context) -> StepOutcome {
    let files = norm_targets(ctx);
    if files.is_empty() {
        return skip_outcome(
            "対象のファイルがありません",
            Some("設定 `c42checker.normExclude` または `targetDir` を確認してください"),
        );
    }
    let bin = ctx.tools.norminette.as_deref().unwrap();
    let mut args = step_args(ctx, StepId::Norminette);
    for f in &files {
        let rel = f.strip_prefix(&ctx.root).unwrap_or(f);
        args.push(rel.to_string_lossy().into_owned());
    }
    let res = exec_cmd(
        bin,
        &args,
        &ExecOptions {
            cwd: &ctx.root,
            timeout_ms: ctx.settings.static_timeout_sec * 1000,
            cancelled: Some(ctx.cancelled.clone()),
            force_c_locale: true,
        },
    );
    let mut output = strip_ansi(&res.output);
    let mut code = res.code;
    if ctx.settings.norminette_ignore_parameter_errors {
        let dropped = drop_norminette_errors(&output, NORMINETTE_PARAMETER_CODES);
        output = dropped.text;
        if dropped.dropped > 0 && dropped.remaining == 0 && code == Some(1) {
            code = Some(0);
        }
    }
    let diags = parse_norminette(&output, &ctx.root);
    let log = if diags.is_empty() && code == Some(0) {
        format!("問題はありません ({} ファイル)\n", files.len())
    } else {
        let lines: Vec<String> = diags
            .iter()
            .map(|d| format!("{}:{}:{}: {}", d.file, d.line, d.col, d.message))
            .collect();
        if lines.is_empty() {
            output
        } else {
            format!("{}\n", lines.join("\n"))
        }
    };
    verdict(code, diags, log, res.error, res.timed_out)
}

fn run_norm_extra_step(ctx: &Context) -> StepOutcome {
    let files = norm_targets(ctx);
    if files.is_empty() {
        return skip_outcome("対象のファイルがありません", None);
    }
    let res = run_norm_extra(ctx, &files);
    StepOutcome {
        status: if res.has_error {
            crate::types::StepStatus::Fail
        } else {
            crate::types::StepStatus::Pass
        },
        diags: res.diags,
        log: res.log,
        reason: if res.has_error {
            Some("Norm (*) 規則のエラーがあります".to_string())
        } else {
            None
        },
        hint: None,
    }
}

fn run_makefile_step(ctx: &Context) -> StepOutcome {
    if ctx.settings.ignore_makefile {
        return skip_outcome("設定 `c42checker.ignoreMakefile` が ON のためスキップしました", None);
    }
    let Some(mk) = find_makefile(&ctx.target_root) else {
        return skip_outcome("Makefile が見つかりません", None);
    };
    let run = check_makefile(ctx, &mk);
    let diags = makefile_diags(&mk, &run.findings);
    let has_err = diags.iter().any(|d| d.severity == Severity::Error);
    StepOutcome {
        status: if has_err {
            crate::types::StepStatus::Fail
        } else {
            crate::types::StepStatus::Pass
        },
        diags,
        log: run.log,
        reason: if has_err {
            Some("Makefile (Norm III.11) に問題があります".to_string())
        } else {
            run.note
        },
        hint: None,
    }
}

fn run_warnings(ctx: &Context) -> StepOutcome {
    if ctx.sources.is_empty() {
        return skip_outcome("対象の .c ファイルがありません", None);
    }
    let cc = ctx.tools.cc.as_deref().unwrap();
    let mut args = vec!["-fsyntax-only".to_string()];
    args.extend(ctx.settings.compile_warning_flags.clone());
    args.extend(inc_flags(ctx));
    args.extend(extra_cflags(ctx));
    args.extend(step_args(ctx, StepId::Warnings));
    for s in &ctx.sources {
        args.push(s.to_string_lossy().into_owned());
    }
    let res = exec_cmd(
        cc,
        &args,
        &ExecOptions {
            cwd: &ctx.root,
            timeout_ms: ctx.settings.static_timeout_sec * 1000,
            cancelled: Some(ctx.cancelled.clone()),
            force_c_locale: true,
        },
    );
    let diags = parse_compiler_output(&res.output, &ctx.root, "clang");
    let log = format!("$ {cc} {}\n{}", args.join(" "), res.output);
    verdict(res.code, diags, log, res.error, res.timed_out)
}

fn run_clang_tidy(ctx: &Context) -> StepOutcome {
    if ctx.sources.is_empty() {
        return skip_outcome("対象の .c ファイルがありません", None);
    }
    let tidy = ctx.tools.tidy.as_deref().unwrap();
    let mut args = vec![
        format!("-checks=-*,{}", ctx.settings.clang_tidy_checks),
        "--quiet".to_string(),
    ];
    args.extend(step_args(ctx, StepId::ClangTidy));
    for s in &ctx.sources {
        args.push(s.to_string_lossy().into_owned());
    }
    args.push("--".to_string());
    args.extend(inc_flags(ctx));
    args.extend(extra_cflags(ctx));

    let res = exec_cmd(
        tidy,
        &args,
        &ExecOptions {
            cwd: &ctx.root,
            timeout_ms: ctx.settings.static_timeout_sec * 1000,
            cancelled: Some(ctx.cancelled.clone()),
            force_c_locale: true,
        },
    );
    let diags = parse_compiler_output(&res.output, &ctx.root, "clang-tidy");
    let log = format!("$ {tidy} {}\n{}", args.join(" "), res.output);
    verdict(res.code, diags, log, res.error, res.timed_out)
}

fn run_scan_build(ctx: &Context) -> StepOutcome {
    if ctx.sources.is_empty() {
        return skip_outcome("対象の .c ファイルがありません", None);
    }
    let sb = ctx.tools.scan_build.as_deref().unwrap();
    let report_dir = ctx.work_dir.join("scan-build-report");
    let _ = fs::create_dir_all(&report_dir);

    let mut args = vec![
        "-o".to_string(),
        report_dir.to_string_lossy().into_owned(),
        "--status-bugs".to_string(),
    ];
    args.extend(step_args(ctx, StepId::ScanBuild));

    if ctx.has_makefile {
        let tmp_copy = ctx.work_dir.join("scan-build-copy");
        let _ = fs::remove_dir_all(&tmp_copy);
        let max_bytes = ctx.settings.work_dir_max_mb * 1024 * 1024;
        if let Err(e) = copy_project(&ctx.target_root, &tmp_copy, max_bytes) {
            return StepOutcome {
                status: crate::types::StepStatus::Fail,
                diags: Vec::new(),
                log: e.clone(),
                reason: Some(e),
                hint: None,
            };
        }
        let mk = find_makefile(&tmp_copy);
        let use_check = ctx.settings.use_make_check_target
            && mk.as_ref().is_some_and(|m| make_target_exists(m, "check"));
        args.push("make".to_string());
        if use_check {
            args.push("check".to_string());
        }
        let res = exec_cmd(
            sb,
            &args,
            &ExecOptions {
                cwd: &tmp_copy,
                timeout_ms: ctx.settings.static_timeout_sec * 1000,
                cancelled: Some(ctx.cancelled.clone()),
                force_c_locale: true,
            },
        );
        let _ = fs::remove_dir_all(&tmp_copy);
        let diags = parse_compiler_output(&res.output, &ctx.target_root, "scan-build");
        let log = format!("$ {sb} {}\n{}", args.join(" "), res.output);
        return verdict(res.code, diags, log, res.error, res.timed_out);
    }

    let cc = ctx.tools.cc.as_deref().unwrap_or("clang");
    args.push(cc.to_string());
    args.push("-fsyntax-only".to_string());
    args.extend(inc_flags(ctx));
    args.extend(extra_cflags(ctx));
    for s in &ctx.sources {
        args.push(s.to_string_lossy().into_owned());
    }
    let res = exec_cmd(
        sb,
        &args,
        &ExecOptions {
            cwd: &ctx.root,
            timeout_ms: ctx.settings.static_timeout_sec * 1000,
            cancelled: Some(ctx.cancelled.clone()),
            force_c_locale: true,
        },
    );
    let diags = parse_compiler_output(&res.output, &ctx.root, "scan-build");
    let log = format!("$ {sb} {}\n{}", args.join(" "), res.output);
    verdict(res.code, diags, log, res.error, res.timed_out)
}

fn run_gcc_analyzer(ctx: &Context) -> StepOutcome {
    if ctx.sources.is_empty() {
        return skip_outcome("対象の .c ファイルがありません", None);
    }
    let gcc = ctx.tools.gcc.as_deref().unwrap();
    let mut args = vec![
        "-fanalyzer".to_string(),
        "-fsyntax-only".to_string(),
        "-Werror=analyzer-possible-null-dereference".to_string(),
        "-Werror=analyzer-null-dereference".to_string(),
        "-Werror=analyzer-use-after-free".to_string(),
        "-Werror=analyzer-double-free".to_string(),
    ];
    args.extend(inc_flags(ctx));
    args.extend(extra_cflags(ctx));
    args.extend(step_args(ctx, StepId::GccAnalyzer));
    for s in &ctx.sources {
        args.push(s.to_string_lossy().into_owned());
    }
    let res = exec_cmd(
        gcc,
        &args,
        &ExecOptions {
            cwd: &ctx.root,
            timeout_ms: ctx.settings.static_timeout_sec * 1000,
            cancelled: Some(ctx.cancelled.clone()),
            force_c_locale: true,
        },
    );
    let diags = parse_compiler_output(&res.output, &ctx.root, "gcc-analyzer");
    let log = format!("$ {gcc} {}\n{}", args.join(" "), res.output);
    verdict(res.code, diags, log, res.error, res.timed_out)
}

fn run_binary_with_optional_setarch(
    ctx: &Context,
    bin: &Path,
    use_setarch: bool,
) -> crate::exec::ExecResult {
    let run_args = normalize_args(&ctx.settings.run_args, &ctx.root);
    let timeout_ms = ctx.settings.run_timeout_sec * 1000;
    if use_setarch {
        if let Some(ref setarch) = ctx.tools.setarch {
            let mut args = machine_arch();
            args.push("-R".to_string());
            args.push(bin.to_string_lossy().into_owned());
            args.extend(run_args.clone());
            let r = exec_cmd(
                setarch,
                &args,
                &ExecOptions {
                    cwd: &ctx.root,
                    timeout_ms,
                    cancelled: Some(ctx.cancelled.clone()),
                    force_c_locale: true,
                },
            );
            if r.error.is_none() {
                return r;
            }
        }
    }
    exec_cmd(
        &bin.to_string_lossy(),
        &run_args,
        &ExecOptions {
            cwd: &ctx.root,
            timeout_ms,
            cancelled: Some(ctx.cancelled.clone()),
            force_c_locale: true,
        },
    )
}

fn run_sanitizer_step(
    ctx: &Context,
    step_id: StepId,
    flags: &[&str],
    dir_name: &str,
    use_setarch: bool,
) -> StepOutcome {
    if !build_inputs(ctx).usable {
        return skip_outcome(NO_MAIN_REASON, Some(NO_MAIN_HINT));
    }
    let out_dir = ctx.work_dir.join(dir_name);
    let b = build_binary(ctx, &out_dir, flags, "clang", Some(step_id));
    if !b.ok {
        return StepOutcome {
            status: crate::types::StepStatus::Fail,
            diags: b.diags,
            log: b.output,
            reason: Some("ビルドに失敗しました".to_string()),
            hint: None,
        };
    }
    let res = run_binary_with_optional_setarch(ctx, &b.bin, use_setarch);
    let diags = parse_sanitizer(&res.output, &ctx.root);
    let log = format!("{}\n$ {}\n{}", b.output, b.bin.display(), res.output);
    verdict(res.code, diags, log, res.error, res.timed_out)
}

fn run_valgrind_step(ctx: &Context) -> StepOutcome {
    if !build_inputs(ctx).usable {
        return skip_outcome(NO_MAIN_REASON, Some(NO_MAIN_HINT));
    }
    let out_dir = ctx.work_dir.join("valgrind");
    let b = build_binary(ctx, &out_dir, &["-g", "-O0"], "clang", None);
    if !b.ok {
        return StepOutcome {
            status: crate::types::StepStatus::Fail,
            diags: b.diags,
            log: b.output,
            reason: Some("ビルドに失敗しました".to_string()),
            hint: None,
        };
    }
    let vg = ctx.tools.valgrind.as_deref().unwrap();
    let mut args = vec!["--error-exitcode=42".to_string()];
    args.extend(ctx.settings.valgrind_args.clone());
    args.extend(step_args(ctx, StepId::Valgrind));
    args.push(b.bin.to_string_lossy().into_owned());
    args.extend(normalize_args(&ctx.settings.run_args, &ctx.root));

    let res = exec_cmd(
        vg,
        &args,
        &ExecOptions {
            cwd: &ctx.root,
            timeout_ms: ctx.settings.run_timeout_sec * 1000 * 3,
            cancelled: Some(ctx.cancelled.clone()),
            force_c_locale: true,
        },
    );
    let parsed = parse_valgrind(&res.output, &ctx.root);
    let log = format!("{}\n$ {vg} {}\n{}", b.output, args.join(" "), res.output);
    let mut out = verdict(res.code, parsed.diags, log, res.error, res.timed_out);
    if parsed.fd_leaks > 0 {
        out.status = crate::types::StepStatus::Fail;
        out.reason = Some(format!("未クローズのファイルディスクリプタが {} 件あります", parsed.fd_leaks));
    }
    out
}

fn run_cbmc_step(ctx: &Context) -> StepOutcome {
    if ctx.sources.is_empty() {
        return skip_outcome("対象の .c ファイルがありません", None);
    }
    let cbmc = ctx.tools.cbmc.as_deref().unwrap();
    let mut args = vec![
        "--bounds-check".to_string(),
        "--pointer-check".to_string(),
        "--memory-leak-check".to_string(),
    ];
    args.extend(inc_flags(ctx));
    args.extend(extra_cflags(ctx));
    args.extend(step_args(ctx, StepId::Cbmc));
    for s in &ctx.sources {
        args.push(s.to_string_lossy().into_owned());
    }
    let res = exec_cmd(
        cbmc,
        &args,
        &ExecOptions {
            cwd: &ctx.root,
            timeout_ms: ctx.settings.static_timeout_sec * 1000,
            cancelled: Some(ctx.cancelled.clone()),
            force_c_locale: true,
        },
    );
    let diags = parse_cbmc(&res.output, &ctx.root);
    let log = format!("$ {cbmc} {}\n{}", args.join(" "), res.output);
    verdict(res.code, diags, log, res.error, res.timed_out)
}

fn run_frama_c_step(ctx: &Context) -> StepOutcome {
    if ctx.sources.is_empty() {
        return skip_outcome("対象の .c ファイルがありません", None);
    }
    let fc = ctx.tools.frama_c.as_deref().unwrap();
    let mut args = vec!["-eva".to_string()];
    args.extend(step_args(ctx, StepId::FramaC));
    for s in &ctx.sources {
        args.push(s.to_string_lossy().into_owned());
    }
    let res = exec_cmd(
        fc,
        &args,
        &ExecOptions {
            cwd: &ctx.root,
            timeout_ms: ctx.settings.static_timeout_sec * 1000,
            cancelled: Some(ctx.cancelled.clone()),
            force_c_locale: true,
        },
    );
    let diags = parse_frama_c(&res.output, &ctx.root);
    let log = format!("$ {fc} {}\n{}", args.join(" "), res.output);
    verdict(res.code, diags, log, res.error, res.timed_out)
}

pub fn selected_steps_from_settings(ctx: &Context) -> Vec<StepId> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for id in StepId::ALL {
        if ctx.settings.checks.get(&id).copied().unwrap_or(false) && seen.insert(id) {
            out.push(id);
        }
    }
    out
}
