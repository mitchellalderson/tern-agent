//! Where `shell` commands run: the `SandboxBackend` trait and the host backend.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("failed to start command: {0}")]
    Spawn(std::io::Error),
    #[error("waiting for command: {0}")]
    Wait(std::io::Error),
}

#[derive(Debug, Clone)]
pub struct ExecOpts {
    pub cwd: PathBuf,
    pub timeout: Duration,
    /// Per-stream cap on captured bytes.
    pub max_output: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOutput {
    /// `None` if killed by a signal, timeout, or cancellation.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
    pub truncated: bool,
}

#[async_trait]
pub trait SandboxBackend: Send + Sync {
    async fn exec(
        &self,
        cmd: &str,
        opts: ExecOpts,
        cancel: CancellationToken,
    ) -> Result<ExecOutput, SandboxError>;
}

/// Environment variables passed through to commands. Everything else
/// (including every secret) is dropped.
const PASSTHROUGH_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TMPDIR",
    "TZ",
    "SYSTEMROOT",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
];

/// Runs commands directly on this machine; approvals are the safety layer.
pub struct HostBackend;

#[async_trait]
impl SandboxBackend for HostBackend {
    async fn exec(
        &self,
        cmd: &str,
        opts: ExecOpts,
        cancel: CancellationToken,
    ) -> Result<ExecOutput, SandboxError> {
        let mut command = if cfg!(windows) {
            let mut c = tokio::process::Command::new("cmd");
            c.arg("/C").arg(cmd);
            c
        } else {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c").arg(cmd);
            c
        };
        command
            .current_dir(&opts.cwd)
            .env_clear()
            .envs(
                PASSTHROUGH_ENV
                    .iter()
                    .filter_map(|k| std::env::var_os(k).map(|v| (k, v))),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(SandboxError::Spawn)?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let max = opts.max_output;
        let out_task = tokio::spawn(read_capped(stdout, max));
        let err_task = tokio::spawn(read_capped(stderr, max));

        let mut result = ExecOutput::default();
        tokio::select! {
            status = child.wait() => {
                result.exit_code = status.map_err(SandboxError::Wait)?.code();
            }
            () = tokio::time::sleep(opts.timeout) => {
                result.timed_out = true;
                let _ = child.kill().await;
            }
            () = cancel.cancelled() => {
                result.cancelled = true;
                let _ = child.kill().await;
            }
        }
        // Background processes may hold the pipes open; don't wait forever.
        let grace = Duration::from_secs(2);
        let (out, out_trunc) = tokio::time::timeout(grace, out_task)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        let (err, err_trunc) = tokio::time::timeout(grace, err_task)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        result.stdout = out;
        result.stderr = err;
        result.truncated = out_trunc || err_trunc;
        Ok(result)
    }
}

/// Read everything, keeping at most `max` bytes. Returns (text, truncated).
async fn read_capped<R: AsyncRead + Unpin>(reader: Option<R>, max: usize) -> (String, bool) {
    let Some(mut reader) = reader else {
        return (String::new(), false);
    };
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = max.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
                truncated |= n > room;
            }
        }
    }
    (String::from_utf8_lossy(&kept).into_owned(), truncated)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn opts(dir: &std::path::Path) -> ExecOpts {
        ExecOpts {
            cwd: dir.to_path_buf(),
            timeout: Duration::from_secs(10),
            max_output: 1024,
        }
    }

    #[tokio::test]
    async fn runs_in_cwd_and_captures_output() {
        let tmp = tempfile::tempdir().unwrap();
        let out = HostBackend
            .exec(
                "pwd; echo err >&2; exit 3",
                opts(tmp.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(3));
        let pwd = std::fs::canonicalize(tmp.path()).unwrap();
        assert_eq!(std::fs::canonicalize(out.stdout.trim()).unwrap(), pwd);
        assert_eq!(out.stderr.trim(), "err");
    }

    #[tokio::test]
    async fn environment_is_scrubbed() {
        let tmp = tempfile::tempdir().unwrap();
        // CARGO_* vars are set for every test process; none may leak through.
        let out = HostBackend
            .exec("env", opts(tmp.path()), CancellationToken::new())
            .await
            .unwrap();
        assert!(!out.stdout.contains("CARGO_"), "{}", out.stdout);
        assert!(out.stdout.contains("PATH="));
    }

    #[tokio::test]
    async fn times_out() {
        let tmp = tempfile::tempdir().unwrap();
        let mut o = opts(tmp.path());
        o.timeout = Duration::from_millis(200);
        let out = HostBackend
            .exec("sleep 5", o, CancellationToken::new())
            .await
            .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
    }

    #[tokio::test]
    async fn cancels() {
        let tmp = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c.cancel();
        });
        let out = HostBackend
            .exec("sleep 5", opts(tmp.path()), cancel)
            .await
            .unwrap();
        assert!(out.cancelled);
    }

    #[tokio::test]
    async fn caps_output() {
        let tmp = tempfile::tempdir().unwrap();
        let out = HostBackend
            .exec(
                "head -c 5000 /dev/zero | tr '\\0' a",
                opts(tmp.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.stdout.len(), 1024);
        assert!(out.truncated);
    }
}
