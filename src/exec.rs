use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ExecOptions<'a> {
    pub cwd: &'a Path,
    pub timeout_ms: u64,
    pub cancelled: Option<Arc<AtomicBool>>,
    pub force_c_locale: bool,
}

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub output: String,
    pub timed_out: bool,
    pub error: Option<String>,
}

pub fn exec_cmd(cmd: &str, args: &[String], opts: &ExecOptions<'_>) -> ExecResult {
    let mut command = Command::new(cmd);
    command
        .args(args)
        .current_dir(opts.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if opts.force_c_locale {
        command.env("LC_ALL", "C");
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ExecResult {
                code: None,
                stdout: String::new(),
                stderr: String::new(),
                output: String::new(),
                timed_out: false,
                error: Some(format!("起動に失敗しました ({cmd}): {e}")),
            };
        }
    };

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    let t_out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut p) = stdout_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let t_err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut p) = stderr_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    });

    let start = Instant::now();
    let timeout = Duration::from_millis(opts.timeout_ms.max(100));
    let mut timed_out = false;
    let mut cancelled = false;
    let mut exit_code = None;

    loop {
        if let Some(ref flag) = opts.cancelled {
            if flag.load(Ordering::Relaxed) {
                cancelled = true;
                let _ = child.kill();
                break;
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code();
                break;
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    timed_out = true;
                    let _ = child.kill();
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(_) => break,
        }
    }

    let _ = child.wait();
    let stdout = t_out.join().unwrap_or_default();
    let stderr = t_err.join().unwrap_or_default();
    let output = if stdout.is_empty() {
        stderr.clone()
    } else if stderr.is_empty() {
        stdout.clone()
    } else if stdout.ends_with('\n') {
        format!("{stdout}{stderr}")
    } else {
        format!("{stdout}\n{stderr}")
    };

    let error = if cancelled {
        Some("中止されました".to_string())
    } else if timed_out {
        Some(format!("制限時間 ({}ms) を超えました", opts.timeout_ms))
    } else {
        None
    };

    ExecResult {
        code: exit_code,
        stdout,
        stderr,
        output,
        timed_out,
        error,
    }
}
