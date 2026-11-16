use crate::build::{build_binary, build_inputs, debug_flags, NO_MAIN_HINT, NO_MAIN_REASON};
use crate::prepare::prepare;
use crate::steps::run_step_by_id;
use crate::types::{Context, FailFast, StepId, StepOutcome, StepResult, StepStatus};
use crate::workdir::dir_size;
use serde::Serialize;
use std::time::Instant;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoppedAt {
    pub stage: u8,
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotRunStep {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineReport {
    pub ok: bool,
    pub results: Vec<StepResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<StoppedAt>,
    pub not_run: Vec<NotRunStep>,
    pub skipped: Vec<StepResult>,
}

pub struct PipelineRequest {
    pub selected: Vec<StepId>,
    pub debug_build: bool,
    pub debug_build_only: bool,
}

pub trait PipelineHooks {
    fn on_stage(&mut self, _stage: u8, _label: &str) {}
    fn on_step_start(&mut self, _id: &str, _label: &str) {}
    fn on_step_end(&mut self, _result: &StepResult) {}
}

pub struct NoopHooks;
impl PipelineHooks for NoopHooks {}

pub fn build_debug(ctx: &Context) -> StepResult {
    let t0 = Instant::now();
    let label = "デバッグ用ビルド";
    if !build_inputs(ctx).usable {
        return StepResult::from_outcome(
            "debug-build",
            label,
            3,
            0,
            StepOutcome {
                status: StepStatus::Fail,
                diags: Vec::new(),
                log: String::new(),
                reason: Some(NO_MAIN_REASON.to_string()),
                hint: Some(NO_MAIN_HINT.to_string()),
            },
        );
    }
    let flags = debug_flags(ctx);
    let b = build_binary(ctx, &ctx.work_dir.join("debug"), &flags, "clang", None);
    let ms = t0.elapsed().as_millis() as u64;
    StepResult::from_outcome(
        "debug-build",
        label,
        3,
        ms,
        StepOutcome {
            status: if b.ok { StepStatus::Pass } else { StepStatus::Fail },
            diags: b.diags,
            log: b.output,
            reason: if b.ok {
                None
            } else {
                Some("デバッグ用ビルドに失敗しました".to_string())
            },
            hint: None,
        },
    )
}

fn record_step(
    r: StepResult,
    hooks: &mut dyn PipelineHooks,
    results: &mut Vec<StepResult>,
    stopped: &mut Option<StoppedAt>,
) {
    hooks.on_step_end(&r);
    if r.status == StepStatus::Fail && stopped.is_none() {
        *stopped = Some(StoppedAt {
            stage: r.stage,
            id: r.id.clone(),
            label: r.label.clone(),
            reason: r.reason.clone(),
        });
    }
    results.push(r);
}

pub fn run_pipeline(
    ctx: &Context,
    req: &PipelineRequest,
    hooks: &mut dyn PipelineHooks,
) -> PipelineReport {
    let mut results = Vec::new();
    let mut stopped: Option<StoppedAt> = None;

    let selected: Vec<StepId> = StepId::ALL
        .iter()
        .copied()
        .filter(|id| req.selected.contains(id))
        .collect();

    // Stage 0: Prepare
    hooks.on_stage(0, "準備");
    hooks.on_step_start("prepare", "準備");
    let t0 = Instant::now();
    let prep = prepare(ctx);
    record_step(
        StepResult::from_outcome(
            "prepare",
            "準備",
            0,
            t0.elapsed().as_millis() as u64,
            prep,
        ),
        hooks,
        &mut results,
        &mut stopped,
    );

    let finish = |results: Vec<StepResult>, stopped: Option<StoppedAt>| -> PipelineReport {
        let not_run: Vec<NotRunStep> = selected
            .iter()
            .filter(|id| !results.iter().any(|r| r.id == id.as_str()))
            .map(|id| NotRunStep {
                id: id.as_str().to_string(),
                label: id.label().to_string(),
            })
            .collect();
        let skipped: Vec<StepResult> = results
            .iter()
            .filter(|r| r.status == StepStatus::Skip)
            .cloned()
            .collect();
        PipelineReport {
            ok: stopped.is_none(),
            results,
            stopped_at: stopped,
            not_run,
            skipped,
        }
    };

    if stopped.is_some() {
        return finish(results, stopped);
    }

    let max_bytes = ctx.settings.work_dir_max_mb * 1024 * 1024;

    if !req.debug_build_only {
        for stage in [1u8, 2u8] {
            let stage_steps: Vec<StepId> = selected
                .iter()
                .copied()
                .filter(|id| id.stage() == stage)
                .collect();
            if stage_steps.is_empty() {
                continue;
            }
            hooks.on_stage(stage, &format!("段階 {stage}"));
            for id in stage_steps {
                if ctx.is_cancelled() {
                    record_step(
                        StepResult::from_outcome(
                            id.as_str(),
                            id.label(),
                            stage,
                            0,
                            StepOutcome {
                                status: StepStatus::Fail,
                                diags: Vec::new(),
                                log: String::new(),
                                reason: Some("中止されました".to_string()),
                                hint: None,
                            },
                        ),
                        hooks,
                        &mut results,
                        &mut stopped,
                    );
                    return finish(results, stopped);
                }
                hooks.on_step_start(id.as_str(), id.label());
                let st0 = Instant::now();
                let out = run_step_by_id(ctx, id);
                record_step(
                    StepResult::from_outcome(
                        id.as_str(),
                        id.label(),
                        stage,
                        st0.elapsed().as_millis() as u64,
                        out,
                    ),
                    hooks,
                    &mut results,
                    &mut stopped,
                );

                let size = dir_size(&ctx.work_dir);
                if size > max_bytes {
                    let mb = (size + 1_048_575) / 1_048_576;
                    record_step(
                        StepResult::from_outcome(
                            "prepare",
                            ".c42checker/ の容量",
                            stage,
                            0,
                            StepOutcome {
                                status: StepStatus::Fail,
                                diags: Vec::new(),
                                log: format!(".c42checker/ が {mb}MB で上限 {}MB を超えました\n", ctx.settings.work_dir_max_mb),
                                reason: Some(format!(".c42checker/ が上限 {}MB を超えました ({mb}MB)", ctx.settings.work_dir_max_mb)),
                                hint: Some("設定 `c42checker.workDirMaxMB` を見直してください".to_string()),
                            },
                        ),
                        hooks,
                        &mut results,
                        &mut stopped,
                    );
                    return finish(results, stopped);
                }

                if stopped.is_some() && ctx.settings.fail_fast == FailFast::Step {
                    return finish(results, stopped);
                }
            }
            if stopped.is_some() {
                return finish(results, stopped);
            }
        }
    }

    if req.debug_build || req.debug_build_only {
        hooks.on_stage(3, "デバッグ用ビルド");
        hooks.on_step_start("debug-build", "デバッグ用ビルド");
        let dbg = build_debug(ctx);
        record_step(dbg, hooks, &mut results, &mut stopped);
    }

    finish(results, stopped)
}
