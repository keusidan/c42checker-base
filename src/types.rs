use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepId {
    CFormatter,
    Norminette,
    NormExtra,
    Makefile,
    Warnings,
    ClangTidy,
    ScanBuild,
    GccAnalyzer,
    AsanUbsan,
    Tsan,
    Msan,
    Valgrind,
    Cbmc,
    FramaC,
}

impl StepId {
    pub const ALL: [StepId; 14] = [
        StepId::CFormatter,
        StepId::Norminette,
        StepId::NormExtra,
        StepId::Makefile,
        StepId::Warnings,
        StepId::ClangTidy,
        StepId::ScanBuild,
        StepId::GccAnalyzer,
        StepId::AsanUbsan,
        StepId::Tsan,
        StepId::Msan,
        StepId::Valgrind,
        StepId::Cbmc,
        StepId::FramaC,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            StepId::CFormatter => "cFormatter",
            StepId::Norminette => "norminette",
            StepId::NormExtra => "normExtra",
            StepId::Makefile => "makefile",
            StepId::Warnings => "warnings",
            StepId::ClangTidy => "clangTidy",
            StepId::ScanBuild => "scanBuild",
            StepId::GccAnalyzer => "gccAnalyzer",
            StepId::AsanUbsan => "asanUbsan",
            StepId::Tsan => "tsan",
            StepId::Msan => "msan",
            StepId::Valgrind => "valgrind",
            StepId::Cbmc => "cbmc",
            StepId::FramaC => "framaC",
        }
    }

    pub fn from_str_id(s: &str) -> Option<StepId> {
        Self::ALL.iter().copied().find(|id| id.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            StepId::CFormatter => "c_formatter_42 (整形)",
            StepId::Norminette => "norminette",
            StepId::NormExtra => "Norm (*) 規則",
            StepId::Makefile => "Makefile (Norm III.11)",
            StepId::Warnings => "警告強化ビルド (clang)",
            StepId::ClangTidy => "clang-tidy",
            StepId::ScanBuild => "scan-build",
            StepId::GccAnalyzer => "gcc -fanalyzer",
            StepId::AsanUbsan => "ASan + UBSan",
            StepId::Tsan => "ThreadSanitizer",
            StepId::Msan => "MemorySanitizer",
            StepId::Valgrind => "valgrind",
            StepId::Cbmc => "cbmc",
            StepId::FramaC => "frama-c",
        }
    }

    pub fn stage(self) -> u8 {
        match self {
            StepId::CFormatter
            | StepId::Norminette
            | StepId::NormExtra
            | StepId::Makefile
            | StepId::Warnings
            | StepId::ClangTidy
            | StepId::ScanBuild
            | StepId::GccAnalyzer => 1,
            StepId::AsanUbsan
            | StepId::Tsan
            | StepId::Msan
            | StepId::Valgrind
            | StepId::Cbmc
            | StepId::FramaC => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepStatus {
    Pass,
    Fail,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FailFast {
    Stage,
    Step,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompdbSource {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "files")]
    Files,
    #[serde(rename = "make-n")]
    MakeN,
    #[serde(rename = "clang-MJ")]
    ClangMj,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diag {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub severity: Severity,
    pub message: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutcome {
    pub status: StepStatus,
    pub diags: Vec<Diag>,
    pub log: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub id: String,
    pub label: String,
    pub stage: u8,
    pub ms: u64,
    pub status: StepStatus,
    pub diags: Vec<Diag>,
    pub log: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl StepResult {
    pub fn from_outcome(id: impl Into<String>, label: impl Into<String>, stage: u8, ms: u64, out: StepOutcome) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            stage,
            ms,
            status: out.status,
            diags: out.diags,
            log: out.log,
            reason: out.reason,
            hint: out.hint,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub target_dir: String,
    pub include_paths: Vec<String>,
    pub run_args: Vec<String>,
    pub use_make_check_target: bool,
    pub ignore_makefile: bool,
    pub fail_fast: FailFast,
    #[serde(rename = "minFreeSpaceMB")]
    pub min_free_space_mb: u64,
    #[serde(rename = "workDirMaxMB")]
    pub work_dir_max_mb: u64,
    pub run_timeout_sec: u64,
    pub static_timeout_sec: u64,
    pub main_file: String,
    pub compdb_include_main: bool,
    pub compdb_source: CompdbSource,
    pub debug_sanitizer: bool,
    pub proto_header: String,
    pub proto_source_dir: String,
    pub proto_sync_on_run: bool,
    pub proto_sync_on_save: bool,
    pub proto_skip_existing: bool,
    pub proto_update_existing: bool,
    pub norminette_ignore_parameter_errors: bool,
    pub norm_exclude: Vec<String>,
    pub norm_extra_disable: Vec<String>,
    pub norm_extra_review_as_fail: bool,
    pub norm_extra_remove_unused_includes: bool,
    pub norm_extra_detect_duplicate_includes: bool,
    pub norm_extra_check_parameter_names: bool,
    pub norm_extra_short_names: Vec<String>,
    pub compile_cflags: Vec<String>,
    pub compile_ldflags: Vec<String>,
    pub compile_libs: Vec<String>,
    pub compile_warning_flags: Vec<String>,
    pub compile_per_step: HashMap<StepId, Vec<String>>,
    pub clang_tidy_checks: String,
    pub valgrind_args: Vec<String>,
    pub checks: HashMap<StepId, bool>,
}

pub fn default_checks() -> HashMap<StepId, bool> {
    let mut m = HashMap::new();
    m.insert(StepId::CFormatter, false);
    m.insert(StepId::Norminette, true);
    m.insert(StepId::NormExtra, true);
    m.insert(StepId::Makefile, true);
    m.insert(StepId::Warnings, true);
    m.insert(StepId::ClangTidy, true);
    m.insert(StepId::ScanBuild, true);
    m.insert(StepId::GccAnalyzer, true);
    m.insert(StepId::AsanUbsan, true);
    m.insert(StepId::Tsan, false);
    m.insert(StepId::Msan, false);
    m.insert(StepId::Valgrind, true);
    m.insert(StepId::Cbmc, false);
    m.insert(StepId::FramaC, false);
    m
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            target_dir: ".".to_string(),
            include_paths: Vec::new(),
            run_args: Vec::new(),
            use_make_check_target: false,
            ignore_makefile: false,
            fail_fast: FailFast::Stage,
            min_free_space_mb: 200,
            work_dir_max_mb: 100,
            run_timeout_sec: 10,
            static_timeout_sec: 120,
            main_file: String::new(),
            compdb_include_main: true,
            compdb_source: CompdbSource::Auto,
            debug_sanitizer: false,
            proto_header: String::new(),
            proto_source_dir: String::new(),
            proto_sync_on_run: false,
            proto_sync_on_save: false,
            proto_skip_existing: true,
            proto_update_existing: true,
            norminette_ignore_parameter_errors: false,
            norm_exclude: vec!["main.c".to_string()],
            norm_extra_disable: Vec::new(),
            norm_extra_review_as_fail: false,
            norm_extra_remove_unused_includes: false,
            norm_extra_detect_duplicate_includes: false,
            norm_extra_check_parameter_names: true,
            norm_extra_short_names: vec!["i", "j", "k", "c", "n", "s"]
                .into_iter()
                .map(String::from)
                .collect(),
            compile_cflags: Vec::new(),
            compile_ldflags: Vec::new(),
            compile_libs: Vec::new(),
            compile_warning_flags: vec!["-Wall", "-Wextra", "-Werror", "-Wshadow", "-Wconversion"]
                .into_iter()
                .map(String::from)
                .collect(),
            compile_per_step: HashMap::new(),
            clang_tidy_checks: "clang-analyzer-*,bugprone-*".to_string(),
            valgrind_args: vec!["--leak-check=full", "--show-leak-kinds=all", "--track-fds=yes"]
                .into_iter()
                .map(String::from)
                .collect(),
            checks: default_checks(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tools {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tidy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan_build: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gcc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valgrind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub norminette: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub make: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cbmc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frama_c: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctags: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c_formatter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setarch: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Context {
    pub root: PathBuf,
    pub target_root: PathBuf,
    pub work_dir: PathBuf,
    pub settings: Settings,
    pub tools: Tools,
    pub sources: Vec<PathBuf>,
    pub headers: Vec<PathBuf>,
    pub include_dirs: Vec<PathBuf>,
    pub has_main: bool,
    pub main_file: Option<PathBuf>,
    pub has_makefile: bool,
    pub cancelled: Arc<AtomicBool>,
    pub dirty_files: Vec<PathBuf>,
}

impl Context {
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn is_file_dirty(&self, abs: &std::path::Path) -> bool {
        self.dirty_files.iter().any(|p| p == abs)
    }
}
