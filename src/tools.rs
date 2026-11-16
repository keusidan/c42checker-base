use crate::types::{StepId, Tools};
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub fn find_on_path(name: &str, env_path: &str) -> Option<String> {
    for dir in env_path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let full = Path::new(dir).join(name);
        if let Ok(meta) = fs::metadata(&full) {
            if meta.is_file() && (meta.permissions().mode() & 0o111) != 0 {
                return Some(full.to_string_lossy().into_owned());
            }
        }
    }
    None
}

pub fn find_highest_versioned(base: &str, env_path: &str) -> Option<String> {
    let prefix = format!("{base}-");
    let mut best: Option<(String, u32)> = None;
    for dir in env_path.split(':') {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(rest) = name.strip_prefix(&prefix) {
                if let Ok(ver) = rest.parse::<u32>() {
                    if find_on_path(&name, dir).is_some()
                        && best.as_ref().is_none_or(|(_, b)| ver > *b)
                    {
                        best = Some((name, ver));
                    }
                }
            }
        }
    }
    best.map(|(n, _)| n)
}

fn find_candidate(candidates: &[&str], versioned_base: Option<&str>, env_path: &str) -> Option<String> {
    for &c in candidates {
        if let Some(found) = find_on_path(c, env_path) {
            return Some(found);
        }
    }
    if let Some(base) = versioned_base {
        return find_highest_versioned(base, env_path);
    }
    None
}

pub fn detect_tools() -> Tools {
    let env_path = env::var("PATH").unwrap_or_default();
    detect_tools_in_path(&env_path)
}

pub fn detect_tools_in_path(env_path: &str) -> Tools {
    Tools {
        cc: find_candidate(&["clang-12", "clang"], Some("clang"), env_path),
        tidy: find_candidate(&["clang-tidy-12", "clang-tidy"], Some("clang-tidy"), env_path),
        scan_build: find_candidate(&["scan-build-12", "scan-build"], Some("scan-build"), env_path),
        gcc: find_candidate(&["gcc-12", "gcc"], Some("gcc"), env_path),
        valgrind: find_candidate(&["valgrind"], None, env_path),
        norminette: find_candidate(&["norminette"], None, env_path),
        make: find_candidate(&["make"], None, env_path),
        cbmc: find_candidate(&["cbmc"], None, env_path),
        frama_c: find_candidate(&["frama-c"], None, env_path),
        ctags: find_candidate(&["universal-ctags", "ctags-universal", "ctags"], None, env_path),
        c_formatter: find_candidate(&["c_formatter_42", "c-formatter-42"], None, env_path),
        setarch: find_candidate(&["setarch"], None, env_path),
    }
}

pub fn machine_arch() -> Vec<String> {
    let arch = std::env::consts::ARCH;
    if arch.is_empty() {
        Vec::new()
    } else {
        vec![arch.to_string()]
    }
}

pub fn step_tool_name(id: StepId) -> Option<&'static str> {
    match id {
        StepId::CFormatter => Some("cFormatter"),
        StepId::Norminette => Some("norminette"),
        StepId::NormExtra => None,
        StepId::Makefile => Some("make"),
        StepId::Warnings => Some("cc"),
        StepId::ClangTidy => Some("tidy"),
        StepId::ScanBuild => Some("scanBuild"),
        StepId::GccAnalyzer => Some("gcc"),
        StepId::AsanUbsan | StepId::Tsan | StepId::Msan => Some("cc"),
        StepId::Valgrind => Some("valgrind"),
        StepId::Cbmc => Some("cbmc"),
        StepId::FramaC => Some("framaC"),
    }
}

pub fn install_hint(tool_key: &str) -> &'static str {
    match tool_key {
        "cc" => "clang (校舎: clang-12 系) が PATH にありません。Arch: `sudo pacman -S clang`",
        "tidy" => "clang-tidy (校舎: clang-tidy-12) が PATH にありません。Arch: `sudo pacman -S clang` (clang-tidy 同梱)",
        "scanBuild" => "scan-build (校舎: scan-build-12) が PATH にありません。Arch: `sudo pacman -S clang-analyzer`",
        "gcc" => "gcc (校舎: gcc-12) が PATH にありません。-fanalyzer は GCC 10 以降が必要です",
        "valgrind" => "valgrind が PATH にありません。Arch: `sudo pacman -S valgrind`",
        "norminette" => "norminette が PATH にありません。校舎では標準で入っています。自分の機では `pipx install norminette` 等",
        "make" => "make が PATH にありません",
        "cbmc" => "cbmc が PATH にありません。ネイティブ版のみ対応です (校舎では Docker image を pull しない方針)",
        "framaC" => "frama-c が PATH にありません。ネイティブ版のみ対応です (校舎では Docker image を pull しない方針)",
        "ctags" => "universal-ctags (ctags-universal) が PATH にありません。Arch: `sudo pacman -S ctags` / Ubuntu: `sudo apt install universal-ctags`",
        "setarch" => "setarch (util-linux) が PATH にありません。TSan / MSan は ASLR 無効 (setarch -R) で起動しないと環境によって起動できないことがあります",
        "cFormatter" => "c_formatter_42 が PATH にありません。`pipx install c-formatter-42` 等で入ります",
        _ => "",
    }
}
