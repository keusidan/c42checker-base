use crate::args::extra_cflags;
use crate::exec::{exec_cmd, ExecOptions};
use crate::parse::parse_compiler_output;
use crate::types::Context;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct IncludeUsage {
    pub file: PathBuf,
    pub line: u32,
    pub header: String,
    pub is_system: bool,
    pub used: bool,
}

pub fn list_includes(file: &Path) -> Vec<(u32, String, bool)> {
    let Ok(text) = fs::read_to_string(file) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let Some(after_hash) = trimmed.strip_prefix('#') else {
            continue;
        };
        let after_inc = after_hash.trim_start();
        let Some(rest) = after_inc.strip_prefix("include") else {
            continue;
        };
        if !rest.starts_with(char::is_whitespace) && !rest.starts_with('<') && !rest.starts_with('"') {
            continue;
        }
        let arg = rest.trim();
        if let Some(after_q) = arg.strip_prefix('"') {
            if let Some(end) = after_q.find('"') {
                out.push(((idx + 1) as u32, after_q[..end].to_string(), false));
            }
        } else if let Some(after_lt) = arg.strip_prefix('<') {
            if let Some(end) = after_lt.find('>') {
                out.push(((idx + 1) as u32, after_lt[..end].to_string(), true));
            }
        }
    }
    out
}

fn count_syntax_diags(ctx: &Context, file: &Path, override_content: Option<(&Path, &str)>) -> Option<usize> {
    let cc = ctx.tools.cc.as_deref()?;
    let tmp_dir = ctx.work_dir.join("inc-check");
    let _ = fs::create_dir_all(&tmp_dir);

    let mut args = vec![
        "-fsyntax-only".to_string(),
        "-fno-color-diagnostics".to_string(),
    ];
    for d in &ctx.include_dirs {
        args.push(format!("-I{}", d.display()));
    }
    args.extend(extra_cflags(ctx));

    let target_arg = if let Some((ov_path, ov_text)) = override_content {
        if ov_path == file {
            let tmp_c = tmp_dir.join("probe.c");
            if fs::write(&tmp_c, ov_text).is_err() {
                return None;
            }
            if let Some(parent) = file.parent() {
                args.push(format!("-I{}", parent.display()));
            }
            tmp_c
        } else {
            file.to_path_buf()
        }
    } else {
        file.to_path_buf()
    };

    args.push(target_arg.to_string_lossy().into_owned());
    let res = exec_cmd(
        cc,
        &args,
        &ExecOptions {
            cwd: &ctx.root,
            timeout_ms: 10_000,
            cancelled: Some(ctx.cancelled.clone()),
            force_c_locale: true,
        },
    );
    if res.error.is_some() {
        return None;
    }
    let diags = parse_compiler_output(&res.output, &ctx.root, "clang");
    Some(diags.len())
}

pub fn check_unused_includes_with_compiler(ctx: &Context, files: &[PathBuf]) -> Vec<IncludeUsage> {
    let mut results = Vec::new();
    if ctx.tools.cc.is_none() {
        return results;
    }
    for file in files {
        if ctx.is_cancelled() {
            break;
        }
        let is_header = file.extension().and_then(|e| e.to_str()) == Some("h");
        if is_header {
            // Header files are skipped in direct -fsyntax-only probe unless standalone
            continue;
        }
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        let includes = list_includes(file);
        if includes.is_empty() {
            continue;
        }
        let Some(base_count) = count_syntax_diags(ctx, file, None) else {
            continue;
        };
        if base_count > 0 {
            // Already has syntax errors; skip probing
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        let mut seen = HashSet::new();
        for (line_no, header, is_system) in includes {
            if !seen.insert(format!("{line_no}:{header}")) {
                continue;
            }
            let idx = (line_no as usize).saturating_sub(1);
            if idx >= lines.len() {
                continue;
            }
            // Comment out this include line
            let mut modified: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
            modified[idx] = format!("/* removed include */");
            let new_text = modified.join("\n") + if text.ends_with('\n') { "\n" } else { "" };
            let probe_count = count_syntax_diags(ctx, file, Some((file, &new_text)));
            let used = probe_count.is_none_or(|c| c > base_count);
            results.push(IncludeUsage {
                file: file.clone(),
                line: line_no,
                header,
                is_system,
                used,
            });
        }
    }
    results
}
