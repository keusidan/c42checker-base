use std::path::Path;

/// Simple glob matcher for exclusion patterns.
pub fn matches_pattern(rel_path: &str, pattern: &str) -> bool {
    let rel = rel_path.replace('\\', "/");
    let mut pat = pattern.trim().to_string();
    if pat.is_empty() {
        return false;
    }
    if pat.ends_with('/') {
        pat.push_str("**");
    }
    let trimmed_end = pat.strip_suffix("**").unwrap_or(&pat);
    let has_slash = trimmed_end.contains('/');
    let target = if has_slash {
        rel.as_str()
    } else {
        rel.rsplit('/').next().unwrap_or(rel.as_str())
    };
    if has_slash {
        if let Some(rest) = pat.strip_prefix("./") {
            pat = rest.to_string();
        } else if let Some(rest) = pat.strip_prefix('/') {
            pat = rest.to_string();
        }
    }
    glob_match(pat.as_bytes(), target.as_bytes())
}

fn glob_match(pat: &[u8], text: &[u8]) -> bool {
    let mut pi = 0;
    let mut ti = 0;
    let mut star_pi: Option<usize> = None;
    let mut star_ti = 0;
    let mut double_star = false;

    while ti < text.len() {
        if pi + 1 < pat.len() && pat[pi] == b'*' && pat[pi + 1] == b'*' {
            star_pi = Some(pi + 2);
            star_ti = ti;
            double_star = true;
            pi += 2;
        } else if pi < pat.len() && pat[pi] == b'*' {
            star_pi = Some(pi + 1);
            star_ti = ti;
            double_star = false;
            pi += 1;
        } else if pi < pat.len() && pat[pi] == b'?' && text[ti] != b'/' {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == text[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(sp) = star_pi {
            if !double_star && text[star_ti] == b'/' {
                return false;
            }
            star_ti += 1;
            ti = star_ti;
            pi = sp;
        } else {
            return false;
        }
    }

    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }
    pi == pat.len()
}

pub fn is_excluded(abs_path: &Path, root: &Path, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let rel = abs_path.strip_prefix(root).unwrap_or(abs_path);
    let rel_str = rel.to_string_lossy();
    patterns.iter().any(|p| matches_pattern(&rel_str, p))
}
