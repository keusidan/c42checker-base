use crate::types::{StepId, Tools};
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

fn darwin_python_user_bins(home: &str) -> Vec<String> {
    let py_root = Path::new(home).join("Library").join("Python");
    let Ok(entries) = fs::read_dir(&py_root) else {
        return Vec::new();
    };
    let mut versions: Vec<(Vec<u32>, String)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let parts: Option<Vec<u32>> = name
            .split('.')
            .map(|p| p.parse::<u32>().ok())
            .collect();
        if let Some(nums) = parts {
            if !nums.is_empty() {
                let bin = py_root.join(&name).join("bin");
                versions.push((nums, bin.to_string_lossy().into_owned()));
            }
        }
    }
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    versions.into_iter().map(|(_, p)| p).collect()
}

pub fn augment_darwin_path(base_path: &str, home: Option<&str>) -> String {
    let mut dirs: Vec<String> = base_path
        .split(':')
        .filter(|d| !d.is_empty())
        .map(String::from)
        .collect();
    let mut extras = Vec::new();
    if let Some(h) = home {
        extras.push(format!("{h}/.local/bin"));
        extras.push(format!("{h}/.cargo/bin"));
        extras.extend(darwin_python_user_bins(h));
    }
    extras.extend([
        "/opt/homebrew/bin".to_string(),
        "/opt/homebrew/opt/llvm/bin".to_string(),
        "/usr/local/bin".to_string(),
        "/usr/local/opt/llvm/bin".to_string(),
    ]);
    for d in extras {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    dirs.join(":")
}

pub fn effective_env_path() -> String {
    let base = env::var("PATH").unwrap_or_default();
    if cfg!(target_os = "macos") {
        let home = env::var("HOME").ok();
        augment_darwin_path(&base, home.as_deref())
    } else {
        base
    }
}

static MACOS_SDK_CACHE: OnceLock<Option<String>> = OnceLock::new();

pub fn detect_macos_sdk_root() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    if let Ok(sdk) = env::var("SDKROOT") {
        if !sdk.trim().is_empty() {
            return Some(sdk);
        }
    }
    MACOS_SDK_CACHE
        .get_or_init(|| {
            let out = Command::new("xcrun")
                .arg("--show-sdk-path")
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() { None } else { Some(s) }
        })
        .clone()
}

pub fn is_apple_clang(bin_path: &str) -> bool {
    let Ok(out) = Command::new(bin_path).arg("--version").output() else {
        return false;
    };
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lower = text.to_ascii_lowercase();
    lower.contains("apple clang version")
        || lower.contains("apple llvm version")
        || lower.contains("clang version")
}

pub fn find_all_on_path(name: &str, env_path: &str) -> Vec<String> {
    let mut results = Vec::new();
    for dir in env_path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let full = Path::new(dir).join(name);
        if let Ok(meta) = fs::metadata(&full) {
            if meta.is_file() && (meta.permissions().mode() & 0o111) != 0 {
                let s = full.to_string_lossy().into_owned();
                if !results.contains(&s) {
                    results.push(s);
                }
            }
        }
    }
    results
}

pub fn find_on_path(name: &str, env_path: &str) -> Option<String> {
    find_all_on_path(name, env_path).into_iter().next()
}

pub fn find_highest_versioned_with<F>(base: &str, env_path: &str, mut accept: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
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
                    if let Some(full) = find_on_path(&name, dir) {
                        if best.as_ref().is_none_or(|(_, b)| ver > *b) && accept(&full) {
                            best = Some((full, ver));
                        }
                    }
                }
            }
        }
    }
    best.map(|(full, _)| full)
}

pub fn find_highest_versioned(base: &str, env_path: &str) -> Option<String> {
    find_highest_versioned_with(base, env_path, |_| true)
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

fn detect_gcc(env_path: &str, is_macos: bool) -> Option<String> {
    let accept = |full: &str| !is_macos || !is_apple_clang(full);
    for &c in &["gcc-12", "gcc"] {
        for found in find_all_on_path(c, env_path) {
            if accept(&found) {
                return Some(found);
            }
        }
    }
    find_highest_versioned_with("gcc", env_path, accept)
}

pub fn detect_tools() -> Tools {
    let env_path = effective_env_path();
    detect_tools_in_path(&env_path)
}

pub fn detect_tools_in_path(env_path: &str) -> Tools {
    detect_tools_for_os(env_path, cfg!(target_os = "macos"))
}

pub fn detect_tools_for_os(env_path: &str, is_macos: bool) -> Tools {
    Tools {
        cc: find_candidate(&["clang-12", "clang"], Some("clang"), env_path),
        tidy: find_candidate(&["clang-tidy-12", "clang-tidy"], Some("clang-tidy"), env_path),
        scan_build: find_candidate(&["scan-build-12", "scan-build"], Some("scan-build"), env_path),
        gcc: detect_gcc(env_path, is_macos),
        valgrind: find_candidate(&["valgrind"], None, env_path),
        leaks: find_candidate(&["leaks"], None, env_path),
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
        StepId::Leaks => Some("leaks"),
        StepId::Cbmc => Some("cbmc"),
        StepId::FramaC => Some("framaC"),
    }
}

pub fn install_hint(tool_key: &str) -> &'static str {
    match tool_key {
        "cc" => "clang (校舎: clang-12 系) が PATH にありません。Arch: `sudo pacman -S clang` / macOS: `xcode-select --install`",
        "tidy" => "clang-tidy (校舎: clang-tidy-12) が PATH にありません。Arch: `sudo pacman -S clang` / macOS: `brew install llvm`",
        "scanBuild" => "scan-build (校舎: scan-build-12) が PATH にありません。Arch: `sudo pacman -S clang-analyzer` / macOS: `brew install llvm`",
        "gcc" => "GNU gcc (校舎: gcc-12) が PATH にありません (macOS の /usr/bin/gcc は Apple clang のため非対応)。-fanalyzer は GCC 10 以降が必要です。macOS: `brew install gcc`",
        "valgrind" => "valgrind が PATH にありません。Arch: `sudo pacman -S valgrind` (macOS では valgrind 非対応の環境が多いため、代わりに「leaks (macOS)」を有効にし、valgrind のチェックを外してください)",
        "leaks" => "leaks は macOS 標準のメモリリーク検査ツールです (`xcode-select --install` が必要)。Linux では valgrind を使用してください",
        "norminette" => "norminette が PATH にありません。校舎では標準で入っています。自分の機 (Linux / macOS) では `pipx install norminette` (または `pip3 install --user norminette`) 等",
        "make" => "make が PATH にありません",
        "cbmc" => "cbmc が PATH にありません。ネイティブ版のみ対応です (校舎では Docker image を pull しない方針)",
        "framaC" => "frama-c が PATH にありません。ネイティブ版のみ対応です (校舎では Docker image を pull しない方針)",
        "ctags" => "universal-ctags (ctags-universal) が PATH にありません。Arch: `sudo pacman -S ctags` / Ubuntu: `sudo apt install universal-ctags` / macOS: `brew install universal-ctags`",
        "setarch" => "setarch (util-linux) が PATH にありません。TSan / MSan は ASLR 無効 (setarch -R) で起動しないと環境によって起動できないことがあります",
        "cFormatter" => "c_formatter_42 が PATH にありません。`pipx install c-formatter-42` 等で入ります",
        _ => "",
    }
}
