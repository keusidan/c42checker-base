use crate::build::build_inputs;
use crate::compdb::{ensure_generated, EnsureAction, STATE_DIR};
use crate::proto::sync_prototypes;
use crate::types::{Context, StepOutcome, StepStatus};
use crate::workdir::{ensure_gitignore, free_space_mb, is_on_rclone_mount, reset_work_dir};

pub fn prepare(ctx: &Context) -> StepOutcome {
    let mut log = String::new();
    let mut say = |s: &str| {
        log.push_str(s);
        log.push('\n');
    };

    if is_on_rclone_mount(&ctx.root, None) {
        say("警告: ワークスペースが rclone のマウント上にあります。home 配下 (非マウント) に置いてください。");
    }

    let free = match free_space_mb(&ctx.root) {
        Ok(f) => f,
        Err(e) => {
            say(&format!("空き容量を取得できませんでした: {e}"));
            return StepOutcome {
                status: StepStatus::Fail,
                diags: Vec::new(),
                log,
                reason: Some("空き容量を確認できません".to_string()),
                hint: None,
            };
        }
    };
    say(&format!("空き容量: {free}MB (閾値 {}MB)", ctx.settings.min_free_space_mb));
    if free < ctx.settings.min_free_space_mb {
        return StepOutcome {
            status: StepStatus::Fail,
            diags: Vec::new(),
            log,
            reason: Some(format!(
                "空き容量が {free}MB で、閾値 {}MB を下回っています",
                ctx.settings.min_free_space_mb
            )),
            hint: Some("`df -h ~` で確認し、不要なファイルを消すか設定 `c42checker.minFreeSpaceMB` を見直してください".to_string()),
        };
    }

    if let Err(e) = reset_work_dir(&ctx.work_dir, &[STATE_DIR]) {
        say(&format!(".c42checker/ を準備できませんでした: {e}"));
        return StepOutcome {
            status: StepStatus::Fail,
            diags: Vec::new(),
            log,
            reason: Some(".c42checker/ を準備できません".to_string()),
            hint: None,
        };
    }
    if let Ok(true) = ensure_gitignore(&ctx.root) {
        say(".gitignore に .c42checker/ を追記しました");
    }

    if ctx.sources.is_empty() {
        say(&format!("{} に .c ファイルがありません", ctx.target_root.display()));
        return StepOutcome {
            status: StepStatus::Fail,
            diags: Vec::new(),
            log,
            reason: Some("対象の .c ファイルがありません".to_string()),
            hint: Some("設定 `c42checker.targetDir` を確認してください".to_string()),
        };
    }

    if ctx.settings.proto_sync_on_run {
        let r = sync_prototypes(ctx, None, true, false);
        say(&format!("プロトタイプ同期: [{}] {}", r.status, r.message));
        if r.status == "error" {
            return StepOutcome {
                status: StepStatus::Fail,
                diags: Vec::new(),
                log,
                reason: Some(format!("プロトタイプの同期に失敗しました: {}", r.message)),
                hint: None,
            };
        }
    }

    say(&format!(
        "対象: .c {} 件 / .h {} 件 / include {} 件",
        ctx.sources.len(),
        ctx.headers.len(),
        ctx.include_dirs.len()
    ));
    let inputs = build_inputs(ctx);
    if ctx.has_main {
        say("main: 対象ソース内にあり");
    } else if let Some(ref mf) = ctx.main_file {
        say(&format!("main: なし → mainFile ({}) を動的チェックに追加します", mf.display()));
    } else {
        say("main: なし (動的チェックは skip されます。mainFile を設定してください)");
    }
    if let Some(ref note) = inputs.note {
        say(&format!("注意: {note}"));
    }

    match ensure_generated(ctx, false) {
        Ok(g) => {
            match g.action {
                EnsureAction::Generated => {
                    if let Some(r) = g.result {
                        say(&format!("compile_commands.json を生成しました ({:?}, {} 件)", r.method, r.entries));
                    }
                }
                EnsureAction::Regenerated => {
                    if let Some(r) = g.result {
                        say(&format!("compile_commands.json を作り直しました ({:?}, {} 件)", r.method, r.entries));
                    }
                }
                EnsureAction::Unchanged => {
                    say("compile_commands.json: 変更なし");
                }
                EnsureAction::Foreign => {
                    say("compile_commands.json: 既存のファイル (c42checker 以外が作成) は上書きしませんでした");
                }
            }
            if g.clangd_written {
                say(".clangd を生成しました");
            } else if let Some(reason) = g.clangd_reason {
                say(&format!(".clangd: {reason}"));
            }
        }
        Err(e) => {
            say(&format!("警告: compile_commands.json を生成できませんでした: {e}"));
        }
    }

    StepOutcome {
        status: StepStatus::Pass,
        diags: Vec::new(),
        log,
        reason: None,
        hint: None,
    }
}
