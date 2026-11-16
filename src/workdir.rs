use std::fs;
use std::path::Path;
use std::process::Command;

pub const WORK_DIR_NAME: &str = ".c42checker";

pub fn reset_work_dir(work_dir: &Path, keep: &[&str]) -> Result<(), String> {
    if work_dir.file_name().and_then(|n| n.to_str()) != Some(WORK_DIR_NAME) {
        return Err(format!(
            "安全のため {WORK_DIR_NAME} 以外は削除しません: {}",
            work_dir.display()
        ));
    }
    if keep.is_empty() {
        if work_dir.exists() {
            fs::remove_dir_all(work_dir).map_err(|e| e.to_string())?;
        }
    } else if work_dir.exists() {
        let entries = fs::read_dir(work_dir).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !keep.iter().any(|k| *k == name) {
                let p = entry.path();
                if p.is_dir() {
                    fs::remove_dir_all(&p).map_err(|e| e.to_string())?;
                } else {
                    fs::remove_file(&p).map_err(|e| e.to_string())?;
                }
            }
        }
    }
    fs::create_dir_all(work_dir).map_err(|e| e.to_string())
}

pub fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let Ok(meta) = fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            total += dir_size(&p);
        } else if meta.is_file() {
            total += meta.len();
        }
    }
    total
}

pub fn free_space_mb(dir: &Path) -> Result<u64, String> {
    let out = Command::new("df")
        .arg("-Pm")
        .arg(dir)
        .output()
        .map_err(|e| format!("df の実行に失敗: {e}"))?;
    if !out.status.success() {
        return Err("df が失敗しました".to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1).ok_or_else(|| "df 出力を解析できません".to_string())?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    let avail_mb = cols
        .get(3)
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| "df 出力の空き容量を読み取れません".to_string())?;
    Ok(avail_mb)
}

pub fn is_on_rclone_mount(dir: &Path, mounts_text: Option<&str>) -> bool {
    let text = match mounts_text {
        Some(t) => t.to_string(),
        None => match fs::read_to_string("/proc/mounts") {
            Ok(t) => t,
            Err(_) => return false,
        },
    };
    let real = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let real_str = real.to_string_lossy();
    let mut best: Option<(String, String, String)> = None;
    for line in text.lines() {
        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() < 3 {
            continue;
        }
        let src = parts[0];
        let mount = parts[1].replace("\\040", " ");
        let fstype = parts[2];
        let prefix = if mount.ends_with('/') {
            mount.clone()
        } else {
            format!("{mount}/")
        };
        let inside = real_str == mount || real_str.starts_with(&prefix);
        if inside && best.as_ref().is_none_or(|(m, _, _)| mount.len() > m.len()) {
            best = Some((mount, fstype.to_string(), src.to_string()));
        }
    }
    best.is_some_and(|(_, fstype, src)| {
        fstype.to_ascii_lowercase().contains("rclone")
            || src.to_ascii_lowercase().contains("rclone")
    })
}

pub fn ensure_gitignore(root: &Path) -> Result<bool, String> {
    let file = root.join(".gitignore");
    let text = fs::read_to_string(&file).unwrap_or_default();
    let already = text.lines().any(|l| {
        let t = l.trim();
        t == ".c42checker" || t == "/.c42checker" || t == ".c42checker/" || t == "/.c42checker/"
    });
    if already {
        return Ok(false);
    }
    let sep = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    fs::write(&file, format!("{text}{sep}{WORK_DIR_NAME}/\n")).map_err(|e| e.to_string())?;
    Ok(true)
}

pub fn copy_project(src_root: &Path, dest: &Path, max_bytes: u64) -> Result<(), String> {
    let mut copied = 0u64;
    copy_walk(src_root, dest, max_bytes, &mut copied)
}

fn copy_walk(from: &Path, to: &Path, max_bytes: u64, copied: &mut u64) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| e.to_string())?;
    let entries = fs::read_dir(from).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if matches!(
            name.as_str(),
            ".git" | WORK_DIR_NAME | "node_modules" | ".vscode"
        ) || name.ends_with(".o")
            || name.ends_with(".a")
            || name.ends_with(".so")
            || name.ends_with(".d")
        {
            continue;
        }
        let f = entry.path();
        let t = to.join(&name);
        let Ok(meta) = fs::metadata(&f) else {
            continue;
        };
        if meta.is_dir() {
            copy_walk(&f, &t, max_bytes, copied)?;
        } else if meta.is_file() {
            *copied += meta.len();
            if *copied > max_bytes {
                return Err(format!(
                    "コピー対象が上限 ({}MB) を超えました",
                    max_bytes / 1_048_576
                ));
            }
            fs::copy(&f, &t).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
