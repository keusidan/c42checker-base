use crate::sources::{
    collect_include_dirs, defines_main, find_makefile, scan_sources,
};
use crate::tools::detect_tools;
use crate::types::{
    CompdbSource, Context, FailFast, Settings, StepId,
};
use crate::workdir::WORK_DIR_NAME;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToggleDef {
    pub id: &'static str,
    pub label: &'static str,
    pub key: &'static str,
    pub description: &'static str,
    pub default_value: bool,
}

pub const TOGGLES: &[ToggleDef] = &[
    ToggleDef {
        id: "failFastStep",
        label: "failFast を step にする",
        key: "failFast",
        description: "ON: 最初に失敗した項目で止める (\"step\") / OFF: 同じ段階の全項目を実行してから止める (\"stage\")",
        default_value: false,
    },
    ToggleDef {
        id: "debugSanitizer",
        label: "ASan 付きデバッグ",
        key: "debug.sanitizer",
        description: "デバッグ用ビルド (F5) に ASan + UBSan を付ける",
        default_value: false,
    },
    ToggleDef {
        id: "ignoreMakefile",
        label: "Makefile を無視する",
        key: "ignoreMakefile",
        description: "Makefile があっても使わず、compile.* の引数と .c の一覧でビルドして検証する",
        default_value: false,
    },
    ToggleDef {
        id: "useMakeCheckTarget",
        label: "Makefile の check ターゲットを使う",
        key: "useMakeCheckTarget",
        description: "Makefile に check ターゲットがあれば、scan-build は make の代わりに make check を包んで実行する",
        default_value: false,
    },
    ToggleDef {
        id: "protoSyncOnRun",
        label: "実行時 (段階 0) にプロトタイプを同期",
        key: "proto.syncOnRun",
        description: "実行の段階 0 で、ヘッダの auto prototypes の区間を更新する",
        default_value: false,
    },
    ToggleDef {
        id: "protoSyncOnSave",
        label: "保存時にプロトタイプを同期",
        key: "proto.syncOnSave",
        description: ".c を保存したときに、ヘッダの auto prototypes の区間を更新する",
        default_value: false,
    },
    ToggleDef {
        id: "removeUnusedIncludes",
        label: "使っていない #include を削除",
        key: "normExtra.removeUnusedIncludes",
        description: "実行時 (段階 1) に、コンパイラで未使用と確かめた #include を削除する",
        default_value: false,
    },
    ToggleDef {
        id: "detectDuplicateIncludes",
        label: "include の二重指定を検出",
        key: "normExtra.detectDuplicateIncludes",
        description: "同じヘッダを 2 回 include している場合を error にする",
        default_value: false,
    },
];

/// Parse VS Code / Neovim nested `c42checker.*` JSON object or flat Settings JSON.
pub fn parse_settings_value(val: &Value) -> Settings {
    let mut s = Settings::default();
    let Some(obj) = val.as_object() else {
        return s;
    };
    let root = obj.get("c42checker").and_then(|v| v.as_object()).unwrap_or(obj);

    let str_val = |k: &str| root.get(k).and_then(|v| v.as_str()).map(String::from);
    let bool_val = |k: &str| root.get(k).and_then(|v| v.as_bool());
    let u64_val = |k: &str| root.get(k).and_then(|v| v.as_u64());
    let str_arr = |k: &str| {
        root.get(k).and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
    };

    if let Some(v) = str_val("targetDir") { s.target_dir = v; }
    if let Some(v) = str_arr("includePaths") { s.include_paths = v; }
    if let Some(v) = str_arr("runArgs") { s.run_args = v; }
    if let Some(v) = bool_val("useMakeCheckTarget") { s.use_make_check_target = v; }
    if let Some(v) = bool_val("ignoreMakefile") { s.ignore_makefile = v; }
    if let Some(v) = str_val("failFast") {
        s.fail_fast = if v == "step" { FailFast::Step } else { FailFast::Stage };
    }
    if let Some(v) = u64_val("minFreeSpaceMB") { s.min_free_space_mb = v; }
    if let Some(v) = u64_val("workDirMaxMB") { s.work_dir_max_mb = v; }
    if let Some(v) = u64_val("runTimeoutSec") { s.run_timeout_sec = v; }
    if let Some(v) = u64_val("staticTimeoutSec") { s.static_timeout_sec = v; }
    if let Some(v) = str_val("mainFile") { s.main_file = v; }
    if let Some(v) = str_arr("normExclude") { s.norm_exclude = v; }

    // Nested or flat fields: compdb, debug, proto, norminette, normExtra, compile, clangTidy, valgrind, checks
    let get_nested = |group: &str, sub: &str, flat: &str| -> Option<&Value> {
        root.get(group)
            .and_then(|g| g.get(sub))
            .or_else(|| root.get(flat))
    };

    if let Some(v) = get_nested("compdb", "includeMain", "compdbIncludeMain").and_then(|v| v.as_bool()) {
        s.compdb_include_main = v;
    }
    if let Some(v) = get_nested("compdb", "source", "compdbSource").and_then(|v| v.as_str()) {
        s.compdb_source = match v {
            "files" => CompdbSource::Files,
            "make-n" => CompdbSource::MakeN,
            "clang-MJ" => CompdbSource::ClangMj,
            _ => CompdbSource::Auto,
        };
    }
    if let Some(v) = get_nested("debug", "sanitizer", "debugSanitizer").and_then(|v| v.as_bool()) {
        s.debug_sanitizer = v;
    }
    if let Some(v) = get_nested("proto", "header", "protoHeader").and_then(|v| v.as_str()) {
        s.proto_header = v.to_string();
    }
    if let Some(v) = get_nested("proto", "sourceDir", "protoSourceDir").and_then(|v| v.as_str()) {
        s.proto_source_dir = v.to_string();
    }
    if let Some(v) = get_nested("proto", "syncOnRun", "protoSyncOnRun").and_then(|v| v.as_bool()) {
        s.proto_sync_on_run = v;
    }
    if let Some(v) = get_nested("proto", "syncOnSave", "protoSyncOnSave").and_then(|v| v.as_bool()) {
        s.proto_sync_on_save = v;
    }
    if let Some(v) = get_nested("proto", "skipExisting", "protoSkipExisting").and_then(|v| v.as_bool()) {
        s.proto_skip_existing = v;
    }
    if let Some(v) = get_nested("proto", "updateExisting", "protoUpdateExisting").and_then(|v| v.as_bool()) {
        s.proto_update_existing = v;
    }
    if let Some(v) = get_nested("norminette", "ignoreParameterErrors", "norminetteIgnoreParameterErrors").and_then(|v| v.as_bool()) {
        s.norminette_ignore_parameter_errors = v;
    }
    if let Some(arr) = get_nested("normExtra", "disable", "normExtraDisable").and_then(|v| v.as_array()) {
        s.norm_extra_disable = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    if let Some(v) = get_nested("normExtra", "reviewAsFail", "normExtraReviewAsFail").and_then(|v| v.as_bool()) {
        s.norm_extra_review_as_fail = v;
    }
    if let Some(v) = get_nested("normExtra", "removeUnusedIncludes", "normExtraRemoveUnusedIncludes").and_then(|v| v.as_bool()) {
        s.norm_extra_remove_unused_includes = v;
    }
    if let Some(v) = get_nested("normExtra", "detectDuplicateIncludes", "normExtraDetectDuplicateIncludes").and_then(|v| v.as_bool()) {
        s.norm_extra_detect_duplicate_includes = v;
    }
    if let Some(v) = get_nested("normExtra", "checkParameterNames", "normExtraCheckParameterNames").and_then(|v| v.as_bool()) {
        s.norm_extra_check_parameter_names = v;
    }
    if let Some(arr) = get_nested("normExtra", "shortNames", "normExtraShortNames").and_then(|v| v.as_array()) {
        s.norm_extra_short_names = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    if let Some(arr) = get_nested("compile", "cflags", "compileCflags").and_then(|v| v.as_array()) {
        s.compile_cflags = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    if let Some(arr) = get_nested("compile", "ldflags", "compileLdflags").and_then(|v| v.as_array()) {
        s.compile_ldflags = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    if let Some(arr) = get_nested("compile", "libs", "compileLibs").and_then(|v| v.as_array()) {
        s.compile_libs = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    if let Some(arr) = get_nested("compile", "warningFlags", "compileWarningFlags").and_then(|v| v.as_array()) {
        s.compile_warning_flags = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    if let Some(obj) = get_nested("compile", "perStep", "compilePerStep").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if let (Some(id), Some(arr)) = (StepId::from_str_id(k), v.as_array()) {
                s.compile_per_step.insert(
                    id,
                    arr.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                );
            }
        }
    }
    if let Some(v) = get_nested("clangTidy", "checks", "clangTidyChecks").and_then(|v| v.as_str()) {
        s.clang_tidy_checks = v.to_string();
    }
    if let Some(arr) = get_nested("valgrind", "args", "valgrindArgs").and_then(|v| v.as_array()) {
        s.valgrind_args = arr.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    if let Some(obj) = root.get("checks").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            if let (Some(id), Some(b)) = (StepId::from_str_id(k), v.as_bool()) {
                s.checks.insert(id, b);
            }
        }
    }
    s
}

pub fn build_context(
    root: PathBuf,
    settings: Settings,
    cancelled: Arc<AtomicBool>,
    dirty_files: Vec<PathBuf>,
) -> Context {
    let target_root = root.join(&settings.target_dir);
    let work_dir = root.join(WORK_DIR_NAME);
    let scan = scan_sources(&target_root);
    let include_dirs = collect_include_dirs(&root, &target_root, &scan.headers, &settings.include_paths);
    let has_main = scan.sources.iter().any(|f| defines_main(f));
    let main_file = if settings.main_file.trim().is_empty() {
        None
    } else {
        let p = root.join(settings.main_file.trim());
        if p.is_file() { Some(p) } else { None }
    };
    let has_makefile = !settings.ignore_makefile && find_makefile(&target_root).is_some();

    Context {
        root,
        target_root,
        work_dir,
        settings,
        tools: detect_tools(),
        sources: scan.sources,
        headers: scan.headers,
        include_dirs,
        has_main,
        main_file,
        has_makefile,
        cancelled,
        dirty_files,
    }
}
