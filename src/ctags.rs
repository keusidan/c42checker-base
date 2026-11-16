use crate::exec::{exec_cmd, ExecOptions};
use crate::tools::{find_on_path, install_hint};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct CtagsFunction {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub typeref: Option<String>,
    pub signature: Option<String>,
    pub end: Option<u32>,
    pub is_static: bool,
}

pub struct CtagsResolved {
    pub bin: Option<String>,
    pub reason: Option<String>,
    pub hint: Option<String>,
}

pub fn resolve_ctags() -> CtagsResolved {
    let env_path = env::var("PATH").unwrap_or_default();
    let candidates = ["universal-ctags", "ctags-universal", "ctags"];
    let present: Vec<&str> = candidates
        .iter()
        .copied()
        .filter(|n| find_on_path(n, &env_path).is_some())
        .collect();
    if present.is_empty() {
        return CtagsResolved {
            bin: None,
            reason: Some("universal-ctags (ctags-universal) が PATH にありません".to_string()),
            hint: Some(install_hint("ctags").to_string()),
        };
    }
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for name in &present {
        let res = exec_cmd(
            name,
            &["--version".to_string()],
            &ExecOptions {
                cwd: &cwd,
                timeout_ms: 10_000,
                cancelled: None,
                force_c_locale: true,
            },
        );
        if res.output.to_ascii_lowercase().contains("universal ctags") {
            return CtagsResolved {
                bin: Some((*name).to_string()),
                reason: None,
                hint: None,
            };
        }
    }
    CtagsResolved {
        bin: None,
        reason: Some(format!(
            "{} は Universal Ctags ではありません",
            present.join(", ")
        )),
        hint: Some(install_hint("ctags").to_string()),
    }
}

pub struct CtagsOutput {
    pub funcs: Vec<CtagsFunction>,
    pub warnings: Vec<String>,
}

pub fn parse_tags(output: &str, cwd: &Path, kinds: &[&str]) -> CtagsOutput {
    let mut funcs = Vec::new();
    let mut warnings = Vec::new();
    for raw in output.lines() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() || line.starts_with("!_") {
            continue;
        }
        if line.starts_with("ctags") || line.starts_with("universal-ctags") {
            warnings.push(line.to_string());
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 4 {
            continue;
        }
        let name = cols[0];
        let file = cols[1];
        let addr = cols[2];
        let kind = cols[3];
        if !kinds.contains(&kind) {
            continue;
        }
        let line_num = addr
            .trim_end_matches(";\"")
            .parse::<u32>()
            .unwrap_or(0);
        let mut f = CtagsFunction {
            name: name.to_string(),
            file: cwd.join(file),
            line: line_num,
            typeref: None,
            signature: None,
            end: None,
            is_static: false,
        };
        for fld in &cols[4..] {
            if *fld == "file:" {
                f.is_static = true;
            } else if let Some(v) = fld.strip_prefix("typeref:") {
                f.typeref = Some(v.to_string());
            } else if let Some(v) = fld.strip_prefix("signature:") {
                f.signature = Some(v.to_string());
            } else if let Some(v) = fld.strip_prefix("end:") {
                f.end = v.parse::<u32>().ok();
            }
        }
        funcs.push(f);
    }
    CtagsOutput { funcs, warnings }
}

pub fn run_ctags(
    bin: &str,
    files: &[PathBuf],
    cwd: &Path,
    kinds_c: &str,
    kinds: &[&str],
    cancelled: Option<Arc<AtomicBool>>,
) -> Result<CtagsOutput, String> {
    if files.is_empty() {
        return Ok(CtagsOutput {
            funcs: Vec::new(),
            warnings: Vec::new(),
        });
    }
    let mut args = vec![
        "--languages=C".to_string(),
        format!("--kinds-C={kinds_c}"),
        "--fields=+KSStzn".to_string(),
        "--extras=+f".to_string(),
        "-n".to_string(),
        "-o".to_string(),
        "-".to_string(),
    ];
    for f in files {
        args.push(f.to_string_lossy().into_owned());
    }
    let res = exec_cmd(
        bin,
        &args,
        &ExecOptions {
            cwd,
            timeout_ms: 30_000,
            cancelled,
            force_c_locale: true,
        },
    );
    if let Some(err) = res.error {
        return Err(err);
    }
    if res.code != Some(0) {
        return Err(format!("ctags が終了コード {:?} で終了しました: {}", res.code, res.stderr));
    }
    Ok(parse_tags(&res.stdout, cwd, kinds))
}
