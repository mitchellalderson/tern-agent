//! Docker backend: commands run in one long-lived container per workspace,
//! with the workspace bind-mounted at `/workspace`. Shells out to the
//! `docker` CLI (no Docker API client dependency).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::{ExecOpts, ExecOutput, SandboxBackend, SandboxError, run_captured};

pub const CONTAINER_WORKSPACE: &str = "/workspace";
const LABEL: &str = "tern.sandbox=1";

pub struct DockerBackend {
    docker: PathBuf,
    image: String,
    workspace: PathBuf,
    container: String,
    /// Serializes container creation.
    ready: tokio::sync::Mutex<bool>,
}

impl DockerBackend {
    /// `docker` is the CLI to call (normally just `docker`).
    pub fn new(docker: impl Into<PathBuf>, image: &str, workspace: &Path) -> Self {
        let workspace =
            std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
        Self {
            docker: docker.into(),
            image: image.to_owned(),
            container: container_name(&workspace),
            workspace,
            ready: tokio::sync::Mutex::new(false),
        }
    }

    pub fn container(&self) -> &str {
        &self.container
    }

    async fn docker(&self, args: &[&str]) -> Result<(bool, String), SandboxError> {
        let out = Command::new(&self.docker)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(SandboxError::Spawn)?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok((out.status.success(), text.trim().to_owned()))
    }

    /// Make sure our container exists, runs, and uses the configured image.
    async fn ensure(&self) -> Result<(), SandboxError> {
        let mut ready = self.ready.lock().await;
        if *ready {
            return Ok(());
        }
        let (exists, state) = self
            .docker(&[
                "inspect",
                "-f",
                "{{.State.Running}} {{.Config.Image}}",
                &self.container,
            ])
            .await?;
        let fail = |what: &str, out: String| {
            SandboxError::Spawn(std::io::Error::other(format!("docker {what}: {out}")))
        };
        match state.split_once(' ') {
            Some(("true", image)) if exists && image == self.image => {}
            Some(("false", image)) if exists && image == self.image => {
                let (ok, out) = self.docker(&["start", &self.container]).await?;
                if !ok {
                    return Err(fail("start", out));
                }
            }
            _ => {
                if exists {
                    // Image changed in config: replace the container.
                    let _ = self.docker(&["rm", "-f", &self.container]).await?;
                }
                let mount = format!("{}:{CONTAINER_WORKSPACE}", self.workspace.display());
                let mut args = vec![
                    "run",
                    "-d",
                    "--init",
                    "--name",
                    &self.container,
                    "--label",
                    LABEL,
                    "-v",
                    &mount,
                    "-w",
                    CONTAINER_WORKSPACE,
                ];
                let user = owner(&self.workspace);
                if let Some(u) = &user {
                    // Files the sandbox writes stay owned by the user.
                    args.extend(["--user", u.as_str()]);
                }
                args.extend([self.image.as_str(), "sleep", "infinity"]);
                let (ok, out) = self.docker(&args).await?;
                if !ok {
                    return Err(fail("run", out));
                }
            }
        }
        *ready = true;
        Ok(())
    }

    /// Host path → path inside the container (inside the workspace only).
    fn container_cwd(&self, host: &Path) -> String {
        let host = std::fs::canonicalize(host).unwrap_or_else(|_| host.to_path_buf());
        match host.strip_prefix(&self.workspace) {
            Ok(rel) if !rel.as_os_str().is_empty() => {
                format!("{CONTAINER_WORKSPACE}/{}", rel.display())
            }
            _ => CONTAINER_WORKSPACE.to_owned(),
        }
    }
}

/// Stable per workspace, so every run reuses the same container.
fn container_name(workspace: &Path) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in workspace.as_os_str().as_encoded_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("tern-sandbox-{:08x}", h as u32)
}

#[cfg(unix)]
fn owner(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .ok()
        .map(|m| format!("{}:{}", m.uid(), m.gid()))
}

#[cfg(not(unix))]
fn owner(_path: &Path) -> Option<String> {
    None
}

/// Records the command's shell pid inside the container so it can be killed
/// on timeout or cancellation (killing `docker exec` alone leaves it running).
const WRAPPER: &str = r#"echo $$ > "/tmp/tern-$TERN_EXEC_ID.pid"; exec sh -c "$1""#;

#[async_trait]
impl SandboxBackend for DockerBackend {
    async fn exec(
        &self,
        cmd: &str,
        opts: ExecOpts,
        cancel: CancellationToken,
    ) -> Result<ExecOutput, SandboxError> {
        self.ensure().await?;
        let id = format!("{:016x}", fastrand_u64());
        let cwd = self.container_cwd(&opts.cwd);
        let mut command = Command::new(&self.docker);
        command
            .args([
                "exec",
                "-w",
                &cwd,
                "-e",
                &format!("TERN_EXEC_ID={id}"),
                &self.container,
            ])
            .args(["sh", "-c", WRAPPER, "tern-sh", cmd])
            // `docker exec` passes no host environment into the container;
            // the CLI itself only needs enough to find the daemon.
            .env_clear()
            .envs(
                [
                    "PATH",
                    "HOME",
                    "DOCKER_HOST",
                    "DOCKER_CONFIG",
                    "DOCKER_CONTEXT",
                ]
                .iter()
                .filter_map(|k| std::env::var_os(k).map(|v| (k, v))),
            );
        let kill_args = [
            "exec".to_owned(),
            self.container.clone(),
            "sh".to_owned(),
            "-c".to_owned(),
            format!(
                r#"kill -KILL "$(cat /tmp/tern-{id}.pid)" 2>/dev/null; rm -f /tmp/tern-{id}.pid"#
            ),
        ];
        let docker = self.docker.clone();
        let result = run_captured(command, &opts, &cancel, move || async move {
            let _ = tokio::time::timeout(
                Duration::from_secs(10),
                Command::new(&docker)
                    .args(&kill_args)
                    .stdin(Stdio::null())
                    .output(),
            )
            .await;
        })
        .await;
        if result
            .as_ref()
            .is_ok_and(|r| r.exit_code == Some(125) && r.stderr.contains("No such container"))
        {
            // The container vanished (e.g. `docker rm`); recreate next time.
            *self.ready.lock().await = false;
        }
        result
    }
}

/// Process-unique enough for exec ids (no `rand` dependency).
fn fastrand_u64() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or_default();
    t ^ (u64::from(std::process::id()) << 32)
        ^ COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9e3779b97f4a7c15)
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A fake `docker` that logs its arguments and runs `exec`s on the host
    /// with `/workspace` mapped to the real workspace.
    fn fake_docker(dir: &Path, ws: &Path) -> PathBuf {
        let path = dir.join("docker");
        let script = format!(
            r#"#!/bin/sh
echo "$*" >> "{log}"
case "$1" in
  inspect) if [ -f "{state}" ]; then cat "{state}"; else echo "Error: No such object" >&2; exit 1; fi ;;
  run) echo "true $(for a in "$@"; do last=$prev; prev=$a; done; echo $last)" > "{state}"; echo cid ;;
  start) exit 0 ;;
  rm) rm -f "{state}" ;;
  exec)
    shift; cwd=/workspace
    while [ "$#" -gt 0 ]; do
      case "$1" in -w) cwd=$2; shift 2 ;; -e) export "$2"; shift 2 ;; *) break ;; esac
    done
    shift  # container name
    cd "{ws}${{cwd#/workspace}}" && exec "$@" ;;
esac
"#,
            log = dir.join("log").display(),
            state = dir.join("state").display(),
            ws = ws.display(),
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn opts(cwd: &Path, secs: u64) -> ExecOpts {
        ExecOpts {
            cwd: cwd.to_path_buf(),
            timeout: Duration::from_secs(secs),
            max_output: 4096,
            on_output: None,
        }
    }

    #[tokio::test]
    async fn creates_one_container_and_runs_commands_in_it() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        let docker = fake_docker(tmp.path(), &ws);
        let backend = DockerBackend::new(&docker, "debian:stable-slim", &ws);

        let out = backend
            .exec(
                "pwd; echo hi >&2; exit 4",
                opts(&ws.join("sub"), 10),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(4));
        assert!(out.stdout.trim().ends_with("/ws/sub"), "{}", out.stdout);
        assert_eq!(out.stderr.trim(), "hi");
        backend
            .exec("true", opts(&ws, 10), CancellationToken::new())
            .await
            .unwrap();

        let log = std::fs::read_to_string(tmp.path().join("log")).unwrap();
        let runs: Vec<&str> = log.lines().filter(|l| l.starts_with("run ")).collect();
        assert_eq!(runs.len(), 1, "container created once:\n{log}");
        let ws_canon = std::fs::canonicalize(&ws).unwrap();
        assert!(
            runs[0].contains(&format!("-v {}:/workspace", ws_canon.display())),
            "{}",
            runs[0]
        );
        assert!(
            runs[0].contains("--init")
                && runs[0].contains("--user ")
                && runs[0].contains("debian:stable-slim sleep infinity")
        );
        assert!(
            log.contains("exec -w /workspace/sub -e TERN_EXEC_ID="),
            "{log}"
        );
        assert!(runs[0].contains(backend.container()));
    }

    #[tokio::test]
    async fn timeouts_kill_the_process_inside_the_container() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let docker = fake_docker(tmp.path(), &ws);
        let backend = DockerBackend::new(&docker, "img", &ws);
        let out = backend
            .exec("sleep 30", opts(&ws, 1), CancellationToken::new())
            .await
            .unwrap();
        assert!(out.timed_out);
        let log = std::fs::read_to_string(tmp.path().join("log")).unwrap();
        assert!(
            log.lines()
                .any(|l| l.starts_with("exec ") && l.contains("kill -KILL")),
            "{log}"
        );
    }

    #[tokio::test]
    async fn an_image_change_replaces_the_container() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let docker = fake_docker(tmp.path(), &ws);
        DockerBackend::new(&docker, "old:1", &ws)
            .exec("true", opts(&ws, 5), CancellationToken::new())
            .await
            .unwrap();
        DockerBackend::new(&docker, "new:2", &ws)
            .exec("true", opts(&ws, 5), CancellationToken::new())
            .await
            .unwrap();
        let log = std::fs::read_to_string(tmp.path().join("log")).unwrap();
        assert!(
            log.lines().any(|l| l.starts_with("rm -f tern-sandbox-")),
            "{log}"
        );
        assert_eq!(log.lines().filter(|l| l.starts_with("run ")).count(), 2);
    }

    /// Needs a real Docker daemon: `cargo test -p tern-sandbox -- --ignored docker`.
    #[tokio::test]
    #[ignore = "needs a Docker daemon"]
    async fn real_docker_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let backend = DockerBackend::new("docker", "debian:stable-slim", tmp.path());
        let out = backend
            .exec(
                "echo inside > made.txt && cat /etc/os-release | head -1",
                opts(tmp.path(), 120),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0), "{out:?}");
        assert!(out.stdout.contains("Debian"), "{}", out.stdout);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("made.txt"))
                .unwrap()
                .trim(),
            "inside"
        );
        let timed = backend
            .exec("sleep 60", opts(tmp.path(), 2), CancellationToken::new())
            .await
            .unwrap();
        assert!(timed.timed_out);
        // The killed `sleep` is gone from the container.
        let left = backend
            .exec("for p in /proc/[0-9]*; do cat $p/cmdline 2>/dev/null | tr '\\0' ' '; echo; done | grep -c '^sleep 60' || true", opts(tmp.path(), 30), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(left.stdout.trim(), "0", "{left:?}");
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", backend.container()])
            .output();
    }
}
