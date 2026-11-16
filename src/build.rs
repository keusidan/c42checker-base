use crate::args::{extra_cflags, extra_ldflags, extra_libs, step_args};
use crate::exec::{exec_cmd, ExecOptions};
use crate::parse::parse_compiler_output;
use crate::types::{Context, Diag, StepId};
use std::fs;
use std::path::{Path, PathBuf};

pub const NO_MAIN_REASON: &str = "対象ソースに main がありません";
pub const NO_MAIN_HINT: &str =
    "ライブラリ課題 (libft 等) は、テスト用の main (.c) を作り、設定 `c42checker.mainFile` にそのパスを指定してください";

pub struct BuildInputs {
    pub files: Vec<PathBuf>,
    pub usable: bool,
    pub note: Option<String>,
}

pub fn build_inputs(ctx: &Context) -> BuildInputs {
    if ctx.sources.is_empty() {
        return BuildInputs {
            files: Vec::new(),
            usable: false,
            note: None,
        };
    }
    if ctx.has_main {
        return BuildInputs {
            files: ctx.sources.clone(),
            usable: true,
            note: if ctx.main_file.is_some() {
                Some("ソースに main があるため c42checker.mainFile は使いません".to_string())
            } else {
                None
            },
        };
    }
    if let Some(ref mf) = ctx.main_file {
        let mut files = ctx.sources.clone();
        files.push(mf.clone());
        return BuildInputs {
            files,
            usable: true,
            note: None,
        };
    }
    BuildInputs {
        files: ctx.sources.clone(),
        usable: false,
        note: None,
    }
}

pub struct BuildResult {
    pub ok: bool,
    pub bin: PathBuf,
    pub output: String,
    pub diags: Vec<Diag>,
}

pub fn build_binary(
    ctx: &Context,
    out_dir: &Path,
    flags: &[&str],
    source: &str,
    step_id: Option<StepId>,
) -> BuildResult {
    let bin = out_dir.join("prog");
    let _ = fs::create_dir_all(out_dir);
    let Some(ref cc) = ctx.tools.cc else {
        return BuildResult {
            ok: false,
            bin,
            output: "clang が見つかりません\n".to_string(),
            diags: Vec::new(),
        };
    };

    let inputs = build_inputs(ctx);
    let mut args: Vec<String> = flags.iter().map(|s| s.to_string()).collect();
    for d in &ctx.include_dirs {
        args.push(format!("-I{}", d.display()));
    }
    args.extend(extra_cflags(ctx));
    args.extend(extra_ldflags(ctx));
    if let Some(id) = step_id {
        args.extend(step_args(ctx, id));
    }
    args.push("-o".to_string());
    args.push(bin.to_string_lossy().into_owned());
    for f in &inputs.files {
        args.push(f.to_string_lossy().into_owned());
    }
    args.extend(extra_libs(ctx));

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
    let header = format!("$ {cc} {}\n", args.join(" "));
    let mut output = format!("{header}{}", res.output);
    if let Some(ref err) = res.error {
        output.push_str(&format!("{err}\n"));
    }
    let ok = res.code == Some(0) && res.error.is_none() && bin.is_file();
    let diags = parse_compiler_output(&res.output, &ctx.root, source);
    BuildResult {
        ok,
        bin,
        output,
        diags,
    }
}

pub fn debug_flags(ctx: &Context) -> Vec<&'static str> {
    let mut f = vec!["-g", "-O0", "-fno-omit-frame-pointer", "-pthread"];
    if ctx.settings.debug_sanitizer {
        f.push("-fsanitize=address,undefined");
    }
    f
}
