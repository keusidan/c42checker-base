pub mod args;
pub mod build;
pub mod compdb;
pub mod config;
pub mod ctags;
pub mod exclude;
pub mod exec;
pub mod includecheck;
pub mod includefix;
pub mod lsp;
pub mod makefile;
pub mod normextra;
pub mod parse;
pub mod pipeline;
pub mod prepare;
pub mod proto;
pub mod sources;
pub mod steps;
pub mod tools;
pub mod types;
pub mod workdir;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub fn run_entry() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("c42checker-lsp {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.get(1).map(|s| s.as_str()) == Some("--cli") {
        let root = args
            .get(2)
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let ctx = config::build_context(
            root,
            types::Settings::default(),
            Arc::new(AtomicBool::new(false)),
            Vec::new(),
        );
        let selected = steps::selected_steps_from_settings(&ctx);
        let mut hooks = pipeline::NoopHooks;
        let report = pipeline::run_pipeline(
            &ctx,
            &pipeline::PipelineRequest {
                selected,
                debug_build: false,
                debug_build_only: false,
            },
            &mut hooks,
        );
        println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
        std::process::exit(if report.ok { 0 } else { 1 });
    }
    lsp::run_lsp_server();
}
