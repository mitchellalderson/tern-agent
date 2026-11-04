//! The host backend's OS sandbox (docs/sandbox-design.md), through the real
//! `tern` binary: Seatbelt on macOS, the `__sandbox-exec` helper (Landlock and
//! seccomp) on Linux. On a Linux kernel without Landlock these tests check
//! only what `confine = "auto"` promises there: commands still run.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::path::{Path, PathBuf};
use std::time::Duration;

use tern_sandbox::{Confinement, ExecOpts, ExecOutput, HostBackend, HostPolicy, SandboxBackend};
use tokio_util::sync::CancellationToken;

fn tern_bin() -> PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

/// A scratch dir outside the temp dirs (which commands may write) and, in a
/// normal checkout, inside `$HOME`.
fn outside() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

fn backend(policy: HostPolicy) -> Option<HostBackend> {
    let b = HostBackend::new(HostPolicy {
        helper: Some(tern_bin()),
        ..policy
    });
    match b.confinement() {
        Confinement::Seatbelt | Confinement::Landlock { .. } => Some(b),
        other => {
            eprintln!("skipping: no OS sandbox here ({other})");
            None
        }
    }
}

async fn run(b: &HostBackend, ws: &Path, cmd: &str) -> ExecOutput {
    run_for(b, ws, cmd, Duration::from_secs(20)).await
}

async fn run_for(b: &HostBackend, ws: &Path, cmd: &str, timeout: Duration) -> ExecOutput {
    b.exec(
        cmd,
        ExecOpts {
            cwd: ws.to_path_buf(),
            timeout,
            max_output: 64 * 1024,
            on_output: None,
            network: false,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap()
}

fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

#[tokio::test]
async fn writes_are_confined_to_the_workspace() {
    let Some(b) = backend(HostPolicy::confined()) else {
        return;
    };
    let ws = tempfile::tempdir().unwrap();
    let out = run(&b, ws.path(), "echo inside > made.txt && cat made.txt").await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(out.stdout.trim(), "inside");

    let elsewhere = outside();
    let target = elsewhere.path().join("escaped.txt");
    let out = run(&b, ws.path(), &format!("echo x > {}", sh_quote(&target))).await;
    assert_ne!(out.exit_code, Some(0), "{out:?}");
    assert!(
        !target.exists(),
        "a write outside the workspace went through"
    );
}

#[tokio::test]
async fn home_secrets_are_unreadable() {
    let Some(b) = backend(HostPolicy::confined()) else {
        return;
    };
    let ws = tempfile::tempdir().unwrap();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
    let ssh = home.join(".ssh");
    if ssh.is_dir() {
        let out = run(&b, ws.path(), "ls ~/.ssh").await;
        assert_ne!(out.exit_code, Some(0), "listed ~/.ssh: {out:?}");
    }
    // A stand-in key under $HOME, for machines without ~/.ssh.
    let elsewhere = outside();
    let key = elsewhere.path().join(".ssh/id_test");
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    std::fs::write(&key, "SECRET KEY").unwrap();
    let in_home = std::fs::canonicalize(&key)
        .unwrap()
        .starts_with(std::fs::canonicalize(&home).unwrap());
    if cfg!(target_os = "linux") || in_home {
        let out = run(&b, ws.path(), &format!("cat {}", sh_quote(&key))).await;
        assert_ne!(out.exit_code, Some(0), "{out:?}");
        assert!(!out.stdout.contains("SECRET"), "{out:?}");
    }
}

#[tokio::test]
async fn read_paths_are_readable_but_denied_paths_never() {
    let extra = outside();
    std::fs::write(extra.path().join("tool.txt"), "tool").unwrap();
    std::fs::create_dir_all(extra.path().join("data")).unwrap();
    std::fs::write(extra.path().join("data/secrets.toml"), "SECRET").unwrap();
    let Some(b) = backend(HostPolicy {
        read_paths: vec![extra.path().to_path_buf()],
        deny_paths: vec![extra.path().join("data")],
        ..HostPolicy::confined()
    }) else {
        return;
    };
    let ws = tempfile::tempdir().unwrap();
    let out = run(
        &b,
        ws.path(),
        &format!("cat {}", sh_quote(&extra.path().join("data/secrets.toml"))),
    )
    .await;
    assert!(!out.stdout.contains("SECRET"), "{out:?}");
    if matches!(b.confinement(), Confinement::Seatbelt) {
        // Linux drops a grant that contains a denied path; macOS keeps the
        // rest of it readable.
        let out = run(
            &b,
            ws.path(),
            &format!("cat {}", sh_quote(&extra.path().join("tool.txt"))),
        )
        .await;
        assert_eq!(out.stdout.trim(), "tool", "{out:?}");
    }
}

#[tokio::test]
async fn network_off_blocks_ip_connections() {
    if which("curl").is_none() {
        eprintln!("skipping: no curl");
        return;
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let cmd = format!("curl -s -m 2 http://127.0.0.1:{port}/; echo rc=$?");
    let accepted = |l: &std::net::TcpListener| l.accept().is_ok();

    let Some(off) = backend(HostPolicy {
        network: false,
        ..HostPolicy::confined()
    }) else {
        return;
    };
    let ws = tempfile::tempdir().unwrap();
    run(&off, ws.path(), &cmd).await;
    assert!(!accepted(&listener), "connected with network = false");

    // One command may ask for network while the default stays off.
    off.exec(
        &cmd,
        ExecOpts {
            cwd: ws.path().to_path_buf(),
            timeout: Duration::from_secs(10),
            max_output: 64 * 1024,
            on_output: None,
            network: true,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(
        accepted(&listener),
        "no connection for a command given network"
    );
    run(&off, ws.path(), &cmd).await;
    assert!(!accepted(&listener), "the next command is offline again");

    let on = backend(HostPolicy::confined()).unwrap();
    run(&on, ws.path(), &cmd).await;
    assert!(accepted(&listener), "no connection with network = true");
}

#[tokio::test]
async fn timeout_kills_the_whole_process_group() {
    let Some(b) = backend(HostPolicy::confined()) else {
        return;
    };
    let ws = tempfile::tempdir().unwrap();
    let out = run_for(
        &b,
        ws.path(),
        "sleep 30 & echo $! > child.pid; wait",
        Duration::from_millis(1500),
    )
    .await;
    assert!(out.timed_out, "{out:?}");
    let pid = std::fs::read_to_string(ws.path().join("child.pid")).unwrap();
    let pid = pid.trim();
    let mut gone = false;
    for _ in 0..50 {
        // `kill -0` succeeds on zombies; `ps -o stat` shows them as Z.
        let ps = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&ps.stdout);
        if !ps.status.success() || stat.trim().starts_with('Z') {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "background child {pid} outlived the timeout");
}

#[tokio::test]
async fn everyday_tools_still_work() {
    let Some(b) = backend(HostPolicy {
        read_paths: vec![
            PathBuf::from(std::env::var_os("HOME").unwrap()).join(".gitconfig"),
            PathBuf::from(std::env::var_os("HOME").unwrap()).join(".config/git"),
        ],
        ..HostPolicy::confined()
    }) else {
        return;
    };
    let ws = tempfile::tempdir().unwrap();
    let out = run(
        &b,
        ws.path(),
        "mkdir -p a/b && echo hi > a/b/c && mv a/b/c a/d && ls a | sort | tr '\\n' ' ' && echo tmp > \"${TMPDIR:-/tmp}/tern-sbx-$$\" && rm \"${TMPDIR:-/tmp}/tern-sbx-$$\" && echo ok > /dev/null",
    )
    .await;
    assert_eq!(out.exit_code, Some(0), "{out:?}");
    assert_eq!(out.stdout.trim(), "b d");
    if which("git").is_some() {
        let out = run(
            &b,
            ws.path(),
            "git init -q && git status --short && echo ok",
        )
        .await;
        assert_eq!(out.exit_code, Some(0), "{out:?}");
    }
}

fn which(cmd: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(cmd))
        .find(|p| p.is_file())
}
