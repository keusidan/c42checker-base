use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".c42checker",
    "node_modules",
    ".vscode",
    ".cache",
    "out",
    "dist",
];

pub struct SourceScan {
    pub sources: Vec<PathBuf>,
    pub headers: Vec<PathBuf>,
}

pub fn scan_sources(target_root: &Path) -> SourceScan {
    let mut sources = Vec::new();
    let mut headers = Vec::new();
    walk_sources(target_root, &mut sources, &mut headers);
    SourceScan { sources, headers }
}

fn walk_sources(dir: &Path, sources: &mut Vec<PathBuf>, headers: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut list: Vec<_> = entries.flatten().collect();
    list.sort_by_key(|e| e.file_name());
    for entry in list {
        let name = entry.file_name().to_string_lossy().into_owned();
        let full = entry.path();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') {
                continue;
            }
            walk_sources(&full, sources, headers);
        } else if ft.is_file() {
            if name.ends_with(".c") {
                sources.push(full);
            } else if name.ends_with(".h") {
                headers.push(full);
            }
        }
    }
}

/// Replace comments and string literals with spaces while preserving newlines.
pub fn strip_comments_and_strings(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            out.push(b' ');
            out.push(b' ');
            i += 2;
            while i < bytes.len() {
                if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                    break;
                }
                out.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                i += 1;
            }
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            out.push(b' ');
            out.push(b' ');
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
        } else if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            out.push(b' ');
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\n' {
                    break;
                }
                if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] != b'\n' {
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                    continue;
                }
                let c = bytes[i];
                out.push(b' ');
                i += 1;
                if c == quote {
                    break;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn defines_main(file: &Path) -> bool {
    let Ok(raw) = fs::read_to_string(file) else {
        return false;
    };
    let clean = strip_comments_and_strings(&raw);
    let bytes = clean.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        if i > 0 && is_word(bytes[i - 1]) {
            i += 1;
            continue;
        }
        let kw_len = if bytes[i..].starts_with(b"int") {
            3
        } else if bytes[i..].starts_with(b"void") {
            4
        } else {
            i += 1;
            continue;
        };
        let mut j = i + kw_len;
        if j >= bytes.len() || !bytes[j].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if bytes[j..].starts_with(b"main") {
            let mut k = j + 4;
            while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                k += 1;
            }
            if k < bytes.len() && bytes[k] == b'(' {
                return true;
            }
        }
        i += 1;
    }
    false
}

pub fn collect_include_dirs(
    root: &Path,
    target_root: &Path,
    headers: &[PathBuf],
    configured: &[String],
) -> Vec<PathBuf> {
    let mut set = BTreeSet::new();
    for p in configured {
        set.insert(root.join(p));
    }
    set.insert(target_root.to_path_buf());
    for h in headers {
        if let Some(parent) = h.parent() {
            set.insert(parent.to_path_buf());
        }
    }
    set.into_iter().take(64).collect()
}

pub fn find_makefile(dir: &Path) -> Option<PathBuf> {
    for name in ["Makefile", "makefile", "GNUmakefile"] {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

pub fn make_target_exists(makefile: &Path, target: &str) -> bool {
    let Ok(text) = fs::read_to_string(makefile) else {
        return false;
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(target) {
            let trimmed = rest.trim_start();
            if let Some(after_colon) = trimmed.strip_prefix(':') {
                if !after_colon.starts_with('=') {
                    return true;
                }
            }
        }
    }
    false
}
