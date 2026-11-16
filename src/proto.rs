use crate::ctags::{resolve_ctags, run_ctags, CtagsFunction};
use crate::exclude::is_excluded;
use crate::sources::scan_sources;
use crate::types::Context;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

pub const MARKER_BEGIN: &str = "/* ---- auto prototypes begin ---- */";
pub const MARKER_END: &str = "/* ---- auto prototypes end ---- */";
const TAB_WIDTH: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnType {
    pub base: String,
    pub stars: String,
}

pub fn normalize_return_type(typeref: Option<&str>) -> Option<ReturnType> {
    let raw = typeref?.trim();
    let (prefix, rest) = raw.split_once(':')?;
    if !matches!(prefix, "typename" | "struct" | "union" | "enum") {
        return None;
    }
    let mut t = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    if prefix != "typename" {
        t = format!("{prefix} {t}");
    }
    while t.contains("* *") {
        t = t.replace("* *", "**");
    }
    if t.contains('(') || t.contains(')') || t.contains('[') || t.contains(']') {
        return None;
    }
    let trimmed = t.trim_end();
    let star_count = trimmed.chars().rev().take_while(|&c| c == '*').count();
    let stars = "*".repeat(star_count);
    let base = trimmed[..trimmed.len() - star_count].trim().to_string();
    if base.is_empty() {
        return None;
    }
    Some(ReturnType { base, stars })
}

fn split_top_level(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in s.chars() {
        if c == '(' || c == '[' {
            depth += 1;
        } else if c == ')' || c == ']' {
            depth -= 1;
        }
        if c == ',' && depth == 0 {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

pub fn normalize_params(sig: Option<&str>) -> Option<String> {
    let raw = sig?.trim();
    if !raw.starts_with('(') || !raw.ends_with(')') {
        return None;
    }
    let inner = raw[1..raw.len() - 1].trim();
    if inner.is_empty() || inner == "void" {
        return Some("(void)".to_string());
    }
    let parts = split_top_level(inner);
    let formatted: Vec<String> = parts
        .into_iter()
        .map(|p| {
            let mut t = p.split_whitespace().collect::<Vec<_>>().join(" ");
            t = t.replace("* ", "*");
            t = t.replace("(*", "(*");
            t
        })
        .collect();
    Some(format!("({})", formatted.join(", ")))
}

#[derive(Debug, Clone)]
pub struct ProtoEntry {
    pub name: String,
    pub ret: ReturnType,
    pub params: String,
}

impl ProtoEntry {
    pub fn format_norm(&self, min_name_col: usize) -> String {
        let base_len = self.ret.base.len();
        let target_col = min_name_col.max(((base_len / TAB_WIDTH) + 1) * TAB_WIDTH);
        let tabs = ((target_col.saturating_sub(base_len)) + TAB_WIDTH - 1) / TAB_WIDTH;
        let pad = "\t".repeat(tabs.max(1));
        format!(
            "{}{}{}{}{};",
            self.ret.base, pad, self.ret.stars, self.name, self.params
        )
    }
}

pub fn render_block(entries: &[ProtoEntry]) -> Vec<String> {
    let max_base = entries.iter().map(|e| e.ret.base.len()).max().unwrap_or(0);
    let min_col = if max_base == 0 {
        0
    } else {
        ((max_base / TAB_WIDTH) + 1) * TAB_WIDTH
    };
    entries.iter().map(|e| e.format_norm(min_col)).collect()
}

pub fn find_marker_lines(text: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = text.lines().collect();
    let begin = lines.iter().position(|l| l.trim() == MARKER_BEGIN)?;
    let end = lines
        .iter()
        .skip(begin + 1)
        .position(|l| l.trim() == MARKER_END)
        .map(|rel| begin + 1 + rel)?;
    Some((begin, end))
}

pub fn replace_block(text: &str, inner: &[String]) -> Option<String> {
    let (begin, end) = find_marker_lines(text)?;
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = lines[..=begin].iter().map(|s| (*s).to_string()).collect();
    out.extend(inner.iter().cloned());
    out.extend(lines[end..].iter().map(|s| (*s).to_string()));
    let mut joined = out.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    Some(joined)
}

pub fn insert_markers_before_endif(text: &str, inner: &[String]) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let endif_idx = lines
        .iter()
        .rposition(|l| l.trim().starts_with("#endif"))
        .unwrap_or(lines.len());
    let mut out: Vec<String> = lines[..endif_idx].iter().map(|s| (*s).to_string()).collect();
    if !out.is_empty() && !out.last().unwrap().trim().is_empty() {
        out.push(String::new());
    }
    out.push(MARKER_BEGIN.to_string());
    out.extend(inner.iter().cloned());
    out.push(MARKER_END.to_string());
    out.extend(lines[endif_idx..].iter().map(|s| (*s).to_string()));
    let mut joined = out.join("\n");
    if text.ends_with('\n') || !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutcome {
    pub status: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub added: usize,
    pub updated: usize,
    pub unchanged: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

pub fn sync_prototypes(
    ctx: &Context,
    header_rel: Option<&str>,
    apply: bool,
    insert_markers: bool,
) -> SyncOutcome {
    let resolved = resolve_ctags();
    let Some(ctags_bin) = resolved.bin else {
        return SyncOutcome {
            status: "skip".to_string(),
            message: resolved.reason.unwrap_or_else(|| "ctags が見つかりません".to_string()),
            header: None,
            added: 0,
            updated: 0,
            unchanged: true,
            preview: None,
        };
    };

    let target_header: Option<PathBuf> = if let Some(h) = header_rel.filter(|s| !s.is_empty()) {
        Some(ctx.root.join(h))
    } else if !ctx.settings.proto_header.is_empty() {
        Some(ctx.root.join(&ctx.settings.proto_header))
    } else {
        ctx.headers.first().cloned()
    };

    let Some(header_path) = target_header else {
        return SyncOutcome {
            status: "skip".to_string(),
            message: "対象のヘッダファイル (.h) が見つかりません".to_string(),
            header: None,
            added: 0,
            updated: 0,
            unchanged: true,
            preview: None,
        };
    };

    if !header_path.is_file() {
        return SyncOutcome {
            status: "error".to_string(),
            message: format!("ヘッダが見つかりません: {}", header_path.display()),
            header: Some(header_path.to_string_lossy().into_owned()),
            added: 0,
            updated: 0,
            unchanged: true,
            preview: None,
        };
    }

    if apply && ctx.is_file_dirty(&header_path) {
        return SyncOutcome {
            status: "skip".to_string(),
            message: "ヘッダに未保存の変更があるため反映しませんでした".to_string(),
            header: Some(header_path.to_string_lossy().into_owned()),
            added: 0,
            updated: 0,
            unchanged: true,
            preview: None,
        };
    }

    let source_root = if ctx.settings.proto_source_dir.is_empty() {
        ctx.target_root.clone()
    } else {
        ctx.root.join(&ctx.settings.proto_source_dir)
    };
    let scan = scan_sources(&source_root);
    let c_files: Vec<PathBuf> = scan
        .sources
        .into_iter()
        .filter(|f| !is_excluded(f, &ctx.root, &ctx.settings.norm_exclude))
        .collect();

    let defs = match run_ctags(&ctags_bin, &c_files, &ctx.root, "f", &["function"], Some(ctx.cancelled.clone())) {
        Ok(o) => o.funcs,
        Err(e) => {
            return SyncOutcome {
                status: "error".to_string(),
                message: e,
                header: Some(header_path.to_string_lossy().into_owned()),
                added: 0,
                updated: 0,
                unchanged: true,
                preview: None,
            };
        }
    };

    // Declarations in other headers (skip if proto_skip_existing)
    let mut other_decls: HashSet<String> = HashSet::new();
    if ctx.settings.proto_skip_existing {
        let other_headers: Vec<PathBuf> = ctx
            .headers
            .iter()
            .filter(|h| *h != &header_path)
            .cloned()
            .collect();
        if let Ok(other_out) = run_ctags(&ctags_bin, &other_headers, &ctx.root, "p", &["prototype"], None) {
            for f in other_out.funcs {
                other_decls.insert(f.name);
            }
        }
    }

    let header_text = match fs::read_to_string(&header_path) {
        Ok(t) => t,
        Err(e) => {
            return SyncOutcome {
                status: "error".to_string(),
                message: format!("ヘッダの読み込みに失敗: {e}"),
                header: Some(header_path.to_string_lossy().into_owned()),
                added: 0,
                updated: 0,
                unchanged: true,
                preview: None,
            };
        }
    };

    let markers = find_marker_lines(&header_text);
    let mut outside_decls: HashSet<String> = HashSet::new();
    if let Ok(hdr_out) = run_ctags(&ctags_bin, &[header_path.clone()], &ctx.root, "p", &["prototype"], None) {
        for f in hdr_out.funcs {
            let ln = f.line as usize;
            let in_block = markers.is_some_and(|(b, e)| ln > b + 1 && ln < e + 1);
            if !in_block {
                outside_decls.insert(f.name);
            }
        }
    }

    let mut seen_names = HashSet::new();
    let mut entries = Vec::new();
    let mut def_map: HashMap<String, CtagsFunction> = HashMap::new();
    for f in defs {
        if f.is_static || f.name == "main" {
            continue;
        }
        if other_decls.contains(&f.name) {
            continue;
        }
        if ctx.settings.proto_skip_existing && outside_decls.contains(&f.name) {
            continue;
        }
        if !seen_names.insert(f.name.clone()) {
            continue;
        }
        let Some(ret) = normalize_return_type(f.typeref.as_deref()) else {
            continue;
        };
        let Some(params) = normalize_params(f.signature.as_deref()) else {
            continue;
        };
        def_map.insert(f.name.clone(), f.clone());
        entries.push(ProtoEntry {
            name: f.name,
            ret,
            params,
        });
    }

    let block = render_block(&entries);
    if markers.is_none() && !insert_markers {
        return SyncOutcome {
            status: "needs-markers".to_string(),
            message: format!("{} に `{MARKER_BEGIN}` がありません", header_path.display()),
            header: Some(header_path.to_string_lossy().into_owned()),
            added: entries.len(),
            updated: 0,
            unchanged: false,
            preview: Some(block.join("\n")),
        };
    }

    let next_text = if markers.is_some() {
        replace_block(&header_text, &block).unwrap_or_else(|| header_text.clone())
    } else {
        insert_markers_before_endif(&header_text, &block)
    };

    let unchanged = next_text == header_text;
    if apply && !unchanged {
        if let Err(e) = fs::write(&header_path, &next_text) {
            return SyncOutcome {
                status: "error".to_string(),
                message: format!("ヘッダの書き込みに失敗: {e}"),
                header: Some(header_path.to_string_lossy().into_owned()),
                added: 0,
                updated: 0,
                unchanged: true,
                preview: None,
            };
        }
    }

    SyncOutcome {
        status: "ok".to_string(),
        message: if unchanged {
            "変更はありません".to_string()
        } else if apply {
            format!("{} 件のプロトタイプを反映しました", entries.len())
        } else {
            format!("{} 件のプロトタイプをプレビュー生成しました", entries.len())
        },
        header: Some(header_path.to_string_lossy().into_owned()),
        added: entries.len(),
        updated: 0,
        unchanged,
        preview: if apply { None } else { Some(next_text) },
    }
}
