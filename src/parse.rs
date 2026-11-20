use crate::types::{Diag, Severity};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

pub const NORMINETTE_PARAMETER_CODES: &[&str] = &[
    "TOO_MANY_ARGS",
    "NO_ARGS_VOID",
    "MISSING_IDENTIFIER",
    "ARG_TYPE_UKN",
];

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() && !(bytes[i] == b'm' || bytes[i].is_ascii_alphabetic()) {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

pub fn normalize_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(comp);
                }
            }
            c => out.push(c),
        }
    }
    out
}

pub fn resolve_file(file: &str, base_dir: &Path) -> String {
    let p = Path::new(file);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(p)
    };
    normalize_path(&abs).to_string_lossy().into_owned()
}

pub fn dedupe(diags: Vec<Diag>) -> Vec<Diag> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(diags.len());
    for d in diags {
        let k = format!("{}:{}:{}:{:?}:{}", d.file, d.line, d.col, d.severity, d.message);
        if seen.insert(k) {
            out.push(d);
        }
    }
    out
}

/// Parse `<file>:<line>:<col>: (fatal error|error|warning|runtime error): <msg>`
pub fn parse_compiler_output(text: &str, base_dir: &Path, source: &str) -> Vec<Diag> {
    parse_compiler_output_mapped(text, base_dir, source, |p| p)
}

pub fn parse_compiler_output_mapped<F>(text: &str, base_dir: &Path, source: &str, map: F) -> Vec<Diag>
where
    F: Fn(String) -> String,
{
    let mut diags = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some((file, ln, col, kind, msg)) = split_compiler_diag(line) {
            let resolved = map(resolve_file(file, base_dir));
            if kind == "runtime error" {
                diags.push(Diag {
                    file: resolved,
                    line: ln,
                    col,
                    severity: Severity::Error,
                    message: format!("runtime error: {msg}"),
                    source: source.to_string(),
                });
            } else {
                let sev = if kind == "warning" {
                    Severity::Warning
                } else {
                    Severity::Error
                };
                diags.push(Diag {
                    file: resolved,
                    line: ln,
                    col,
                    severity: sev,
                    message: msg.to_string(),
                    source: source.to_string(),
                });
            }
        }
    }
    dedupe(diags)
}

fn split_compiler_diag(line: &str) -> Option<(&str, u32, u32, &str, &str)> {
    for kind in ["fatal error", "error", "warning", "runtime error"] {
        let needle = format!(": {kind}: ");
        if let Some(pos) = line.find(&needle) {
            let prefix = &line[..pos];
            let msg = &line[pos + needle.len()..];
            let mut parts = prefix.rsplitn(3, ':');
            let col_s = parts.next()?;
            let line_s = parts.next()?;
            let file = parts.next()?;
            let ln: u32 = line_s.parse().ok()?;
            let col: u32 = col_s.parse().ok()?;
            if !file.is_empty() {
                return Some((file, ln, col, kind, msg));
            }
        }
    }
    None
}

pub fn parse_norminette(text: &str, base_dir: &Path) -> Vec<Diag> {
    let clean = strip_ansi(text);
    let mut diags = Vec::new();
    let mut current = String::new();
    for raw in clean.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some(f) = line.strip_suffix(": OK!").or_else(|| line.strip_suffix(": Error!")) {
            current = f.to_string();
            continue;
        }
        if current.is_empty() {
            continue;
        }
        let rest = if let Some(r) = line.strip_prefix("Error: ") {
            r
        } else if let Some(r) = line.strip_prefix("Notice: ") {
            r
        } else {
            continue;
        };
        if let Some((code, line_num, col_num, msg)) = parse_norminette_detail(rest) {
            diags.push(Diag {
                file: resolve_file(&current, base_dir),
                line: line_num,
                col: col_num,
                severity: Severity::Error,
                message: format!("{code}: {msg}"),
                source: "norminette".to_string(),
            });
        }
    }
    diags
}

fn parse_norminette_detail(rest: &str) -> Option<(&str, u32, u32, &str)> {
    let paren = rest.find("(line:")?;
    let code = rest[..paren].trim();
    if code.is_empty() || !code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let after = &rest[paren + 6..];
    let col_pos = after.find(", col:")?;
    let ln: u32 = after[..col_pos].trim().parse().ok()?;
    let after_col = &after[col_pos + 6..];
    let end_paren = after_col.find("):")?;
    let col: u32 = after_col[..end_paren].trim().parse().ok()?;
    let msg = after_col[end_paren + 2..].trim_start();
    Some((code, ln, col, msg))
}

pub struct DroppedNorminette {
    pub text: String,
    pub dropped: usize,
    pub remaining: usize,
}

pub fn drop_norminette_errors(text: &str, codes: &[&str]) -> DroppedNorminette {
    let clean = strip_ansi(text);
    let lines: Vec<&str> = clean.lines().collect();
    let mut drop = HashSet::new();
    let mut sections: HashMap<usize, (usize, usize)> = HashMap::new(); // (left, dropped)
    let mut head: Option<usize> = None;
    let mut remaining = 0usize;

    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim_end_matches('\r');
        if line.ends_with(": OK!") || line.ends_with(": Error!") {
            head = Some(i);
            sections.insert(i, (0, 0));
            continue;
        }
        let rest = line
            .strip_prefix("Error: ")
            .or_else(|| line.strip_prefix("Notice: "));
        let Some(rest) = rest else {
            continue;
        };
        let Some(paren) = rest.find("(line:") else {
            continue;
        };
        let code = rest[..paren].trim();
        if codes.contains(&code) {
            drop.insert(i);
            if let Some(h) = head {
                if let Some(sec) = sections.get_mut(&h) {
                    sec.1 += 1;
                }
            }
        } else {
            remaining += 1;
            if let Some(h) = head {
                if let Some(sec) = sections.get_mut(&h) {
                    sec.0 += 1;
                }
            }
        }
    }

    let mut out = Vec::with_capacity(lines.len());
    for (i, raw) in lines.iter().enumerate() {
        if drop.contains(&i) {
            continue;
        }
        if let Some(&(left, dropped_cnt)) = sections.get(&i) {
            if dropped_cnt > 0 && left == 0 {
                if let Some(prefix) = raw.strip_suffix("Error!") {
                    out.push(format!("{prefix}OK!"));
                    continue;
                }
            }
        }
        out.push((*raw).to_string());
    }

    DroppedNorminette {
        text: out.join("\n"),
        dropped: drop.len(),
        remaining,
    }
}

fn is_inside(root: &Path, file: &Path) -> bool {
    file.strip_prefix(root).is_ok() && file != root
}

fn in_root_file(root: &Path, name: &str) -> Option<String> {
    let p = Path::new(name);
    if p.is_absolute() {
        let norm = normalize_path(p);
        if is_inside(root, &norm) {
            return Some(norm.to_string_lossy().into_owned());
        }
        return None;
    }
    let f = normalize_path(&root.join(p));
    if is_inside(root, &f) && f.is_file() {
        Some(f.to_string_lossy().into_owned())
    } else {
        None
    }
}

fn parse_asan_frame(line: &str) -> Option<(&str, u32, u32)> {
    let trimmed = line.trim();
    if !trimmed.starts_with('#') {
        return None;
    }
    // Strip optional trailing ` (prog+0x...)`
    let core = if let Some(idx) = trimmed.find(" (") {
        &trimmed[..idx]
    } else {
        trimmed
    };
    let last_token = core.split_whitespace().last()?;
    let mut parts = last_token.rsplitn(3, ':');
    let p1 = parts.next()?;
    let p2 = parts.next()?;
    if let Some(p3) = parts.next() {
        if let (Ok(ln), Ok(col)) = (p2.parse::<u32>(), p1.parse::<u32>()) {
            return Some((p3, ln, col));
        }
    }
    if let Ok(ln) = p1.parse::<u32>() {
        return Some((p2, ln, 1));
    }
    None
}

pub fn parse_sanitizer(text: &str, root: &Path) -> Vec<Diag> {
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();
    let mut diags = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let label = if let Some(idx) = line.find("ERROR: AddressSanitizer: ") {
            Some(format!("AddressSanitizer: {}", &line[idx + 25..]))
        } else if line.starts_with("Direct leak of ") || line.starts_with("Indirect leak of ") {
            Some(format!("LeakSanitizer: {line}"))
        } else if let Some(idx) = line.find("WARNING: ThreadSanitizer: ") {
            let msg = &line[idx + 26..];
            let clean = msg.split(" (pid=").next().unwrap_or(msg);
            Some(format!("ThreadSanitizer: {clean}"))
        } else if let Some(idx) = line.find("WARNING: MemorySanitizer: ") {
            Some(format!("MemorySanitizer: {}", &line[idx + 26..]))
        } else {
            None
        };
        let Some(msg) = label else {
            continue;
        };
        let end = (i + 1 + 40).min(lines.len());
        for frame_line in &lines[i + 1..end] {
            if let Some((f, ln, col)) = parse_asan_frame(frame_line) {
                if let Some(abs) = in_root_file(root, f) {
                    diags.push(Diag {
                        file: abs,
                        line: ln,
                        col,
                        severity: Severity::Error,
                        message: msg,
                        source: "sanitizer".to_string(),
                    });
                    break;
                }
            }
        }
    }
    for d in parse_compiler_output(text, root, "ubsan") {
        if d.message.starts_with("runtime error") {
            diags.push(d);
        }
    }
    dedupe(diags)
}

pub struct ValgrindParse {
    pub diags: Vec<Diag>,
    pub fd_leaks: usize,
}

pub fn parse_valgrind(text: &str, root: &Path) -> ValgrindParse {
    let lines: Vec<String> = text
        .lines()
        .map(|l| {
            let s = l.trim_end_matches('\r');
            if let Some(rest) = s.strip_prefix("==") {
                if let Some(pos) = rest.find("== ") {
                    return rest[pos + 3..].to_string();
                } else if let Some(pos) = rest.find("==") {
                    return rest[pos + 2..].to_string();
                }
            }
            s.to_string()
        })
        .collect();
    let mut diags = Vec::new();
    let mut fd_leaks = 0usize;
    let is_at = |s: &str| {
        let t = s.trim_start();
        (t.starts_with("at 0x") || t.starts_with("by 0x")) && s.starts_with(char::is_whitespace)
    };
    for i in 0..lines.len() {
        let line = &lines[i];
        if line.is_empty() || line.starts_with(char::is_whitespace) {
            continue;
        }
        if !lines.get(i + 1).is_some_and(|n| is_at(n)) {
            continue;
        }
        if line.starts_with("Open file descriptor ") {
            fd_leaks += 1;
        }
        let end = (i + 1 + 30).min(lines.len());
        for frame in &lines[i + 1..end] {
            if !is_at(frame) {
                break;
            }
            if let (Some(lp), Some(rp)) = (frame.rfind('('), frame.rfind(')')) {
                if lp < rp {
                    let inside = &frame[lp + 1..rp];
                    if !inside.starts_with("in ") {
                        if let Some(colon) = inside.rfind(':') {
                            let file_part = &inside[..colon];
                            if let Ok(ln) = inside[colon + 1..].parse::<u32>() {
                                if let Some(abs) = in_root_file(root, file_part) {
                                    diags.push(Diag {
                                        file: abs,
                                        line: ln,
                                        col: 1,
                                        severity: Severity::Error,
                                        message: format!("valgrind: {line}"),
                                        source: "valgrind".to_string(),
                                    });
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    ValgrindParse {
        diags: dedupe(diags),
        fd_leaks,
    }
}

pub struct LeaksParse {
    pub diags: Vec<Diag>,
    pub leak_count: usize,
    pub leaked_bytes: usize,
}

fn parse_leaks_summary(line: &str) -> Option<(usize, usize)> {
    let rest = line.trim().strip_prefix("Process ")?;
    let colon = rest.find(':')?;
    if !rest[..colon].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let after = rest[colon + 1..].trim_start();
    let mut it = after.split_whitespace();
    let count: usize = it.next()?.parse().ok()?;
    let leak_kw = it.next()?;
    if leak_kw != "leak" && leak_kw != "leaks" {
        return None;
    }
    if it.next()? != "for" {
        return None;
    }
    let bytes: usize = it.next()?.parse().ok()?;
    let tail: Vec<&str> = it.take(3).collect();
    if tail.len() >= 3 && tail[0] == "total" && tail[1] == "leaked" && tail[2].starts_with("bytes") {
        Some((count, bytes))
    } else {
        None
    }
}

fn extract_leaks_frame(line: &str, root: &Path) -> Option<(String, u32)> {
    let mut hit: Option<(String, u32)> = None;
    for tok in line.split(|c: char| c.is_whitespace() || c == '|' || c == '(' || c == ')') {
        if tok.is_empty() {
            continue;
        }
        let cleaned = tok.trim_end_matches([',', ';', '.']);
        let Some(colon) = cleaned.rfind(':') else {
            continue;
        };
        let file_part = &cleaned[..colon];
        if !(file_part.ends_with(".c") || file_part.ends_with(".h")) {
            continue;
        }
        let Ok(ln) = cleaned[colon + 1..].parse::<u32>() else {
            continue;
        };
        if let Some(abs) = in_root_file(root, file_part) {
            hit = Some((abs, ln));
        }
    }
    hit
}

pub fn parse_leaks(text: &str, root: &Path) -> LeaksParse {
    let clean = strip_ansi(text);
    let lines: Vec<&str> = clean.lines().map(|l| l.trim_end_matches('\r')).collect();
    let mut leak_count = 0usize;
    let mut leaked_bytes = 0usize;
    for line in &lines {
        if let Some((c, b)) = parse_leaks_summary(line) {
            leak_count = c;
            leaked_bytes = b;
        }
    }
    let mut diags = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("STACK OF ") {
            if let (Some(q1), Some(q2)) = (rest.find('\''), rest.rfind('\'')) {
                if q1 < q2 {
                    let label = &rest[q1 + 1..q2];
                    let msg = format!("leaks: {label}");
                    let mut hit: Option<(String, u32)> = None;
                    let end = (i + 1 + 40).min(lines.len());
                    for fl in &lines[i + 1..end] {
                        let ft = fl.trim();
                        if ft.starts_with("====") || ft.starts_with("STACK OF ") {
                            break;
                        }
                        if let Some(h) = extract_leaks_frame(fl, root) {
                            hit = Some(h);
                        }
                    }
                    if let Some((file, ln)) = hit {
                        diags.push(Diag {
                            file,
                            line: ln,
                            col: 1,
                            severity: Severity::Error,
                            message: msg,
                            source: "leaks".to_string(),
                        });
                    }
                }
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Leak:") {
            let mut toks = rest.split_whitespace();
            let addr = toks.next().unwrap_or("0x0");
            let size_tok = toks.next().unwrap_or("");
            let bytes_str = size_tok.strip_prefix("size=").unwrap_or("0");
            let msg = format!("leaks: {bytes_str} byte(s) leaked ({addr})");
            let end = (i + 1 + 10).min(lines.len());
            for fl in &lines[i + 1..end] {
                if fl.trim().starts_with("Leak:") {
                    break;
                }
                if let Some((file, ln)) = extract_leaks_frame(fl, root) {
                    diags.push(Diag {
                        file,
                        line: ln,
                        col: 1,
                        severity: Severity::Error,
                        message: msg,
                        source: "leaks".to_string(),
                    });
                    break;
                }
            }
        }
    }
    LeaksParse {
        diags: dedupe(diags),
        leak_count,
        leaked_bytes,
    }
}

pub fn parse_cbmc(text: &str, base_dir: &Path) -> Vec<Diag> {
    let mut diags = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix('[') else {
            continue;
        };
        let Some(rb) = rest.find("] file ") else {
            continue;
        };
        let prop = &rest[..rb];
        let after_file = &rest[rb + 7..];
        let Some(line_kw) = after_file.find(" line ") else {
            continue;
        };
        let file = &after_file[..line_kw];
        let after_line = &after_file[line_kw + 6..];
        let Some(sp) = after_line.find(' ') else {
            continue;
        };
        let Ok(ln) = after_line[..sp].parse::<u32>() else {
            continue;
        };
        let rest_msg = &after_line[sp + 1..];
        let Some(msg) = rest_msg.strip_suffix(": FAILURE") else {
            continue;
        };
        diags.push(Diag {
            file: resolve_file(file, base_dir),
            line: ln,
            col: 1,
            severity: Severity::Error,
            message: format!("cbmc: {msg} ({prop})"),
            source: "cbmc".to_string(),
        });
    }
    dedupe(diags)
}

pub fn parse_frama_c(text: &str, base_dir: &Path) -> Vec<Diag> {
    let mut diags = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("[eva:alarm] ") else {
            continue;
        };
        let Some(warn_pos) = rest.find(": Warning: ") else {
            continue;
        };
        let loc = &rest[..warn_pos];
        let msg = &rest[warn_pos + 11..];
        let Some(colon) = loc.rfind(':') else {
            continue;
        };
        let file = &loc[..colon];
        let Ok(ln) = loc[colon + 1..].parse::<u32>() else {
            continue;
        };
        diags.push(Diag {
            file: resolve_file(file, base_dir),
            line: ln,
            col: 1,
            severity: Severity::Error,
            message: format!("eva: {msg}"),
            source: "frama-c".to_string(),
        });
    }
    dedupe(diags)
}
