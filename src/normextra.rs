use crate::includecheck::{check_unused_includes_with_compiler, list_includes};
use crate::includefix::apply_removals;
use crate::sources::strip_comments_and_strings;
use crate::types::{Context, Diag, Severity};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub struct NormExtraOutcome {
    pub diags: Vec<Diag>,
    pub log: String,
    pub has_error: bool,
}

fn is_disabled(ctx: &Context, rule: &str) -> bool {
    ctx.settings.norm_extra_disable.iter().any(|r| r == rule)
}

fn is_bad_short_name(name: &str, allowed: &[String]) -> bool {
    if name.len() == 1 && !allowed.iter().any(|a| a == name) {
        return true;
    }
    // Pattern like f1, x2, tmp1, var_2
    let bytes = name.as_bytes();
    if let Some(last) = bytes.last() {
        if last.is_ascii_digit() {
            let prefix_len = bytes
                .iter()
                .rev()
                .skip_while(|b| b.is_ascii_digit())
                .count();
            let prefix = &name[..prefix_len];
            if prefix.len() <= 2 || prefix == "func" || prefix == "var" || prefix == "tmp" {
                return true;
            }
        }
    }
    false
}

fn is_non_ascii_comment_line(line: &str) -> bool {
    // Detect non-ASCII (e.g., Japanese) inside comments
    if let Some(pos) = line.find("//") {
        return !line[pos..].is_ascii();
    }
    if let Some(pos) = line.find("/*") {
        return !line[pos..].is_ascii();
    }
    false
}

pub fn run_norm_extra(ctx: &Context, files: &[PathBuf]) -> NormExtraOutcome {
    let mut diags = Vec::new();
    let mut log = String::new();
    let mut unused_removals: Vec<(PathBuf, u32)> = Vec::new();

    for file in files {
        if ctx.is_cancelled() {
            break;
        }
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        let file_str = file.to_string_lossy().into_owned();
        let is_header = file.extension().and_then(|e| e.to_str()) == Some("h");

        // 1. bad-file-name
        if !is_disabled(ctx, "bad-file-name") {
            if let Some(stem) = file.file_stem().and_then(|s| s.to_str()) {
                if is_bad_short_name(stem, &[]) || stem == "test" && files.len() > 3 {
                    // Only flag single-char or numbered file names like f1.c
                    if is_bad_short_name(stem, &[]) {
                        diags.push(Diag {
                            file: file_str.clone(),
                            line: 1,
                            col: 1,
                            severity: Severity::Warning,
                            message: format!("[bad-file-name] ファイル名 `{stem}` が短すぎるか連番です (Norm III.1 (*))"),
                            source: "normExtra".to_string(),
                        });
                    }
                }
            }
        }

        // 2. 42header check
        if !is_disabled(ctx, "42header") {
            let first_lines: Vec<&str> = text.lines().take(11).collect();
            let has_42 = first_lines.len() >= 11
                && first_lines[0].starts_with("/* ***")
                && first_lines.iter().any(|l| l.contains("42.fr") || l.contains("By:"));
            if !has_42 {
                diags.push(Diag {
                    file: file_str.clone(),
                    line: 1,
                    col: 1,
                    severity: Severity::Warning,
                    message: "[42header] 42 ヘッダが見つからないか形式が崩れています (Norm III.6 (*))".to_string(),
                    source: "normExtra".to_string(),
                });
            }
        }

        // 3. Line-by-line checks (define, comments)
        for (idx, line) in text.lines().enumerate() {
            let ln = (idx + 1) as u32;
            let trimmed = line.trim();

            if !is_disabled(ctx, "comment-non-english") && is_non_ascii_comment_line(line) {
                diags.push(Diag {
                    file: file_str.clone(),
                    line: ln,
                    col: 1,
                    severity: Severity::Warning,
                    message: "[comment-non-english] コメントに英語以外の文字が含まれています (Norm III.9 (*))".to_string(),
                    source: "normExtra".to_string(),
                });
            }

            if !is_disabled(ctx, "comment-useless") {
                let lower = trimmed.to_ascii_lowercase();
                if lower.contains("todo") || lower.contains("fixme") {
                    if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
                        diags.push(Diag {
                            file: file_str.clone(),
                            line: ln,
                            col: 1,
                            severity: Severity::Warning,
                            message: "[comment-useless] TODO / FIXME コメントが残っています (Norm III.9 (*))".to_string(),
                            source: "normExtra".to_string(),
                        });
                    }
                }
            }

            if let Some(after_hash) = trimmed.strip_prefix('#') {
                let after = after_hash.trim_start();
                if let Some(def) = after.strip_prefix("define") {
                    let def = def.trim_start();
                    if !is_disabled(ctx, "define-function-like") {
                        if let Some(sp_or_lp) = def.find(|c: char| c.is_whitespace() || c == '(') {
                            if def.as_bytes()[sp_or_lp] == b'(' {
                                diags.push(Diag {
                                    file: file_str.clone(),
                                    line: ln,
                                    col: 1,
                                    severity: Severity::Error,
                                    message: "[define-function-like] 関数形式のマクロは禁止されています (Norm III.7 (*))".to_string(),
                                    source: "normExtra".to_string(),
                                });
                            }
                        }
                    }
                    if !is_disabled(ctx, "define-bypass") {
                        if def.contains("while")
                            || def.contains("for")
                            || def.contains("if")
                            || def.contains("return")
                            || def.contains('?')
                            || def.contains(';')
                        {
                            diags.push(Diag {
                                file: file_str.clone(),
                                line: ln,
                                col: 1,
                                severity: Severity::Error,
                                message: "[define-bypass] norm 回避・制御構文を含む #define は禁止です (Norm III.7 (*))".to_string(),
                                source: "normExtra".to_string(),
                            });
                        }
                    }
                }
            }
        }

        // 4. duplicate-include
        if ctx.settings.norm_extra_detect_duplicate_includes && !is_disabled(ctx, "duplicate-include") {
            let mut seen_inc: HashMap<String, u32> = HashMap::new();
            for (ln, hdr, _) in list_includes(file) {
                if let Some(prev) = seen_inc.insert(hdr.clone(), ln) {
                    diags.push(Diag {
                        file: file_str.clone(),
                        line: ln,
                        col: 1,
                        severity: Severity::Error,
                        message: format!("[duplicate-include] `{hdr}` が {prev} 行目と重複して include されています (Norm III.5 (*))"),
                        source: "normExtra".to_string(),
                    });
                }
            }
        }

        // 5. header-content: check if .h has function bodies (simplified: `)` followed by `{` at top level)
        if is_header && !is_disabled(ctx, "header-content") {
            let clean = strip_comments_and_strings(&text);
            let lines: Vec<&str> = clean.lines().collect();
            for (idx, line) in lines.iter().enumerate() {
                let t = line.trim();
                if t == "{" && idx > 0 && lines[idx - 1].trim_end().ends_with(')') {
                    diags.push(Diag {
                        file: file_str.clone(),
                        line: (idx + 1) as u32,
                        col: 1,
                        severity: Severity::Error,
                        message: "[header-content] ヘッダに関数本体は書けません (Norm III.5 (*))".to_string(),
                        source: "normExtra".to_string(),
                    });
                }
            }
        }
    }

    // 6. unused-include via compiler probe
    if !is_disabled(ctx, "unused-include") {
        let c_files: Vec<PathBuf> = files
            .iter()
            .filter(|f| f.extension().and_then(|e| e.to_str()) == Some("c"))
            .cloned()
            .collect();
        let usages = check_unused_includes_with_compiler(ctx, &c_files);
        for u in usages {
            if !u.used {
                diags.push(Diag {
                    file: u.file.to_string_lossy().into_owned(),
                    line: u.line,
                    col: 1,
                    severity: Severity::Error,
                    message: format!("[unused-include] 使っていないヘッダ `{}` が include されています (Norm III.5 (*))", u.header),
                    source: "normExtra".to_string(),
                });
                unused_removals.push((u.file, u.line));
            }
        }
    }

    if ctx.settings.norm_extra_remove_unused_includes && !unused_removals.is_empty() {
        let (cnt, lines) = apply_removals(ctx, &unused_removals);
        if cnt > 0 {
            log.push_str(&format!("未使用 #include を {cnt} 件削除しました:\n"));
            for l in lines {
                log.push_str(&format!("{l}\n"));
            }
        }
    }

    if ctx.settings.norm_extra_review_as_fail {
        for d in &mut diags {
            if d.severity == Severity::Warning {
                d.severity = Severity::Error;
            }
        }
    }

    let has_error = diags.iter().any(|d| d.severity == Severity::Error);
    if diags.is_empty() {
        log.push_str(&format!("Norm (*) 規則: 問題はありません ({} ファイル)\n", files.len()));
    } else {
        for d in &diags {
            log.push_str(&format!(
                "{}:{}:{}: [{}] {}\n",
                d.file,
                d.line,
                d.col,
                if d.severity == Severity::Error { "error" } else { "warning" },
                d.message
            ));
        }
    }
    log.push_str("\n--- 機械では判定できない項目 (評価で確認) ---\n");
    log.push_str("  [ ] III.1 (*): 全ての識別子名が英語として意味が明確で、単語が _ で区切られている\n");
    log.push_str("  [ ] III.7 (*): 標準ライブラリのマクロが課題で許可された範囲に収まっている\n");

    let _ = Path::new("");
    let _ = HashSet::<String>::new();

    NormExtraOutcome {
        diags,
        log,
        has_error,
    }
}
