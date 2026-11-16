use crate::types::Context;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;

pub fn apply_removals(ctx: &Context, removals: &[(PathBuf, u32)]) -> (usize, Vec<String>) {
    let mut by_file: BTreeMap<PathBuf, HashSet<u32>> = BTreeMap::new();
    for (f, ln) in removals {
        if ctx.is_file_dirty(f) {
            continue;
        }
        by_file.entry(f.clone()).or_default().insert(*ln);
    }

    let backup_dir = ctx.work_dir.join("include-backup");
    let mut removed_count = 0usize;
    let mut log_lines = Vec::new();

    for (file, lines_to_remove) in by_file {
        let Ok(orig) = fs::read_to_string(&file) else {
            continue;
        };
        let rel = file.strip_prefix(&ctx.root).unwrap_or(&file);
        let bak_path = backup_dir.join(rel);
        if let Some(parent) = bak_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&bak_path, &orig);

        let lines: Vec<&str> = orig.lines().collect();
        let mut next = Vec::with_capacity(lines.len());
        let mut count_here = 0usize;
        for (i, line) in lines.iter().enumerate() {
            if lines_to_remove.contains(&((i + 1) as u32)) {
                count_here += 1;
            } else {
                next.push(*line);
            }
        }
        let mut out = next.join("\n");
        if orig.ends_with('\n') && !out.is_empty() {
            out.push('\n');
        }
        if fs::write(&file, out).is_ok() {
            removed_count += count_here;
            log_lines.push(format!(
                "  {}: {} 行の未使用 #include を削除 (退避: .c42checker/include-backup/{})",
                rel.display(),
                count_here,
                rel.display()
            ));
        }
    }

    (removed_count, log_lines)
}
