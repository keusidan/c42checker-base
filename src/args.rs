use crate::types::{Context, StepId};

/// Shell-like splitting supporting single/double quotes and backslash escapes.
pub fn shell_split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut quote: Option<char> = None;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else if c == '\\' && q == '"' && i + 1 < chars.len() {
                i += 1;
                cur.push(chars[i]);
            } else {
                cur.push(c);
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
            has = true;
        } else if c == '\\' && i + 1 < chars.len() {
            i += 1;
            cur.push(chars[i]);
            has = true;
        } else if c.is_whitespace() {
            if has || !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
                has = false;
            }
        } else {
            cur.push(c);
        }
        i += 1;
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn normalize_args(list: &[String], root: &std::path::Path) -> Vec<String> {
    let root_str = root.to_string_lossy();
    let mut out = Vec::new();
    for item in list {
        for tok in shell_split(item) {
            let v = tok.replace("${workspaceFolder}", &root_str);
            if !v.is_empty() {
                out.push(v);
            }
        }
    }
    out
}

pub fn extra_cflags(ctx: &Context) -> Vec<String> {
    normalize_args(&ctx.settings.compile_cflags, &ctx.root)
}

pub fn extra_ldflags(ctx: &Context) -> Vec<String> {
    normalize_args(&ctx.settings.compile_ldflags, &ctx.root)
}

pub fn extra_libs(ctx: &Context) -> Vec<String> {
    normalize_args(&ctx.settings.compile_libs, &ctx.root)
}

pub fn step_args(ctx: &Context, id: StepId) -> Vec<String> {
    ctx.settings
        .compile_per_step
        .get(&id)
        .map(|v| normalize_args(v, &ctx.root))
        .unwrap_or_default()
}
