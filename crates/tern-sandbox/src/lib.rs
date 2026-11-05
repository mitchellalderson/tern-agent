//! Where `shell` commands run: the `SandboxBackend` trait, the host
//! backend, and the docker backend.

mod docker;
mod helper;
mod policy;
mod toolchain;
mod verify;

pub use docker::{DockerBackend, DockerSettings};
pub use helper::{HELPER_ARG, run_if_requested as run_helper_if_requested};
pub use policy::{Confine, HostPolicy};
pub use toolchain::{ToolchainGrant, detect_here as detect_toolchains};
pub use verify::Verification;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("failed to start command: {0}")]
    Spawn(std::io::Error),
    #[error("waiting for command: {0}")]
    Wait(std::io::Error),
    #[error("refusing to run unconfined (sandbox.confine = \"required\"): {0}")]
    Unconfined(String),
    #[error(
        "this backend can't turn networking on for one command; set sandbox.network = true to give every command network"
    )]
    NoNetworkPerCommand,
}

/// Receives output text (stdout and stderr interleaved) while a command runs.
#[derive(Clone)]
pub struct OutputSink(pub Arc<dyn Fn(&str) + Send + Sync>);

impl std::fmt::Debug for OutputSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OutputSink")
    }
}

#[derive(Debug, Clone)]
pub struct ExecOpts {
    pub cwd: PathBuf,
    pub timeout: Duration,
    /// Per-stream cap on captured bytes. Past it the middle is dropped: the
    /// first quarter and the last three quarters are kept, with a marker
    /// (the end of a log is where the failure summary is).
    pub max_output: usize,
    /// Live output, coalesced to a few calls per second. It receives the
    /// stream's first `max_output` bytes; the final `ExecOutput` has the
    /// head and tail.
    pub on_output: Option<OutputSink>,
    /// IP networking for this command even when the backend's default is
    /// off (an approved `shell` call with `network: true`).
    pub network: bool,
    /// Environment set on top of the scrubbed one (the run's workspace-local
    /// `TMPDIR` and tool caches).
    pub env: Vec<(String, String)>,
    /// Extra read-only folders for this command (the workspace's user-added
    /// read roots), granted in addition to the policy's `read_paths`. This is
    /// also the input the macOS read-denial phase builds on: the profile will
    /// deny reads under `/Users` and `/Volumes` except the workspace, these
    /// roots and the detected toolchain directories.
    pub read_roots: Vec<PathBuf>,
}

/// Live output is batched for this long before it is handed to the sink.
const STREAM_INTERVAL: Duration = Duration::from_millis(250);

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

    /// Whether commands can reach the network: allowed by the settings, or
    /// not blocked because nothing confines them. Their output is then
    /// third-party text (tooling spec §5.3).
    fn network_reachable(&self) -> bool {
        true
    }

    /// Whether commands run in a container (so confinement is by mounts and
    /// can't be probed), as opposed to a sandbox on this machine.
    fn is_container(&self) -> bool {
        false
    }

    /// Whether nothing confines commands here, known without probing: the
    /// confinement is off, or wanted but unavailable on this machine.
    fn unconfined(&self) -> bool {
        false
    }

    /// Whether the OS sandbox is confirmed active for commands run in
    /// `workspace` (see [`HostBackend::verify`]). A backend that can't be
    /// probed says so: nothing may waive an approval on its account.
    fn verify(&self, _workspace: &Path) -> Verification {
        Verification::not_verified("this backend's confinement can't be probed")
    }

    /// [`SandboxBackend::verify`], but may block to probe (for `tern doctor`,
    /// `/v1/info` and tests, never the agent loop).
    fn verify_now(&self, workspace: &Path) -> Verification {
        self.verify(workspace)
    }

    /// Run an inspection program inside the sandbox (network off, no extra
    /// writes) and return its stdout; `Err` when this backend can't.
    fn probe_blocking(
        &self,
        _program: Vec<std::ffi::OsString>,
        _cwd: &Path,
    ) -> Result<String, String> {
        Err("this backend can't run a confined inspection".into())
    }

    /// Start whatever [`SandboxBackend::verify`] needs in the background.
    fn warm(&self, _workspace: &Path) {}

    /// Where a command can write, as canonical paths: an interpreter or
    /// other program found there could be replaced by the command itself.
    fn writable_roots(&self, workspace: &Path) -> Vec<PathBuf> {
        vec![workspace.to_path_buf()]
    }
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

/// How the host backend confines commands on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confinement {
    /// `sandbox.confine = "off"`.
    Off,
    /// macOS Seatbelt via `sandbox-exec`.
    Seatbelt,
    /// Linux Landlock at this ABI level (1–6), through the helper.
    Landlock { abi: u8 },
    /// Wanted but not possible here; commands run unconfined (or not at all
    /// with `confine = "required"`).
    Unavailable(String),
}

impl std::fmt::Display for Confinement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("off (sandbox.confine = \"off\")"),
            Self::Seatbelt => f.write_str("macOS Seatbelt"),
            Self::Landlock { abi } => write!(f, "Linux Landlock (ABI {abi})"),
            Self::Unavailable(why) => write!(f, "unavailable: {why}"),
        }
    }
}

#[cfg(target_os = "macos")]
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Runs commands on this machine, inside the OS sandbox `HostPolicy`
/// describes (see `docs/sandbox-design.md`). Approvals come first.
pub struct HostBackend {
    policy: HostPolicy,
    confinement: OnceLock<Confinement>,
    warned: std::sync::atomic::AtomicBool,
}

impl HostBackend {
    pub fn new(policy: HostPolicy) -> Self {
        Self {
            policy,
            confinement: OnceLock::new(),
            warned: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// No OS sandbox: approvals and the scrubbed environment only.
    pub fn unconfined() -> Self {
        Self::new(HostPolicy::unconfined())
    }

    pub fn policy(&self) -> &HostPolicy {
        &self.policy
    }

    /// What confines commands here (probed once; on Linux this runs the
    /// helper).
    pub fn confinement(&self) -> &Confinement {
        self.confinement.get_or_init(|| detect(&self.policy))
    }

    /// The program and arguments that run `cmd` in `cwd` under the sandbox,
    /// with networking when the policy or the command (`network`) asks.
    fn argv(
        &self,
        cmd: &str,
        cwd: &Path,
        network: bool,
        read_roots: &[PathBuf],
    ) -> Result<Vec<std::ffi::OsString>, SandboxError> {
        let adjusted;
        let policy = if (network && !self.policy.network) || !read_roots.is_empty() {
            let mut p = HostPolicy {
                network: self.policy.network || network,
                ..self.policy.clone()
            };
            p.read_paths.extend(read_roots.iter().cloned());
            adjusted = p;
            &adjusted
        } else {
            &self.policy
        };
        let shell: Vec<std::ffi::OsString> = if cfg!(windows) {
            vec!["cmd".into(), "/C".into(), cmd.into()]
        } else {
            vec!["sh".into(), "-c".into(), cmd.into()]
        };
        self.wrap(policy, shell, cwd)
    }

    /// The argv that runs `program` (with its arguments) in `cwd` under this
    /// backend's sandbox, for callers that talk to the process themselves
    /// (the `python` tool's interpreter helper). Errors only when the policy
    /// requires a sandbox that isn't available.
    pub fn confine_argv(
        &self,
        program: Vec<std::ffi::OsString>,
        cwd: &Path,
    ) -> Result<Vec<std::ffi::OsString>, SandboxError> {
        self.wrap(&self.policy, program, cwd)
    }

    /// [`HostBackend::confine_argv`] with extra read-only folders (the run's
    /// user-added read roots, which the interpreter mounts).
    pub fn confine_argv_reading(
        &self,
        program: Vec<std::ffi::OsString>,
        cwd: &Path,
        read_roots: &[PathBuf],
    ) -> Result<Vec<std::ffi::OsString>, SandboxError> {
        if read_roots.is_empty() {
            return self.confine_argv(program, cwd);
        }
        let mut policy = self.policy.clone();
        policy.read_paths.extend(read_roots.iter().cloned());
        self.wrap(&policy, program, cwd)
    }

    fn wrap(
        &self,
        policy: &HostPolicy,
        shell: Vec<std::ffi::OsString>,
        cwd: &Path,
    ) -> Result<Vec<std::ffi::OsString>, SandboxError> {
        match self.confinement() {
            Confinement::Off => Ok(shell),
            Confinement::Unavailable(why) => {
                if self.policy.confine == Confine::Required {
                    return Err(SandboxError::Unconfined(why.clone()));
                }
                if !self.warned.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    tracing::warn!(
                        "shell commands run without an OS sandbox: {why} (set sandbox.confine = \"off\" to silence this)"
                    );
                }
                Ok(shell)
            }
            #[cfg(target_os = "macos")]
            Confinement::Seatbelt => {
                let mut argv: Vec<std::ffi::OsString> = vec![
                    SANDBOX_EXEC.into(),
                    "-p".into(),
                    policy::seatbelt_profile(policy, cwd).into(),
                ];
                argv.extend(shell);
                Ok(argv)
            }
            #[cfg(target_os = "linux")]
            Confinement::Landlock { .. } => {
                let grants = policy::linux_grants(policy, cwd);
                if !grants.dropped.is_empty()
                    && !self.warned.swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    tracing::warn!(
                        dropped = ?grants.dropped,
                        "sandbox paths that contain tern's data dir are not granted"
                    );
                }
                let mut argv: Vec<std::ffi::OsString> =
                    vec![helper_path(&self.policy)?.into(), HELPER_ARG.into()];
                argv.extend(policy::helper_args(policy, &grants));
                argv.push("--".into());
                argv.extend(shell);
                Ok(argv)
            }
            #[allow(unreachable_patterns)]
            other => Err(SandboxError::Unconfined(format!(
                "{other} is not supported on this platform"
            ))),
        }
    }
}

impl Default for HostBackend {
    fn default() -> Self {
        Self::new(HostPolicy::confined())
    }
}

#[cfg(target_os = "linux")]
fn helper_path(policy: &HostPolicy) -> Result<PathBuf, SandboxError> {
    match &policy.helper {
        Some(p) => Ok(p.clone()),
        None => std::env::current_exe().map_err(SandboxError::Spawn),
    }
}

fn detect(policy: &HostPolicy) -> Confinement {
    if policy.confine == Confine::Off {
        return Confinement::Off;
    }
    detect_os(policy)
}

#[cfg(target_os = "macos")]
fn detect_os(_policy: &HostPolicy) -> Confinement {
    if Path::new(SANDBOX_EXEC).exists() {
        Confinement::Seatbelt
    } else {
        Confinement::Unavailable(format!("{SANDBOX_EXEC} not found"))
    }
}

#[cfg(target_os = "linux")]
fn detect_os(policy: &HostPolicy) -> Confinement {
    let helper = match helper_path(policy) {
        Ok(h) => h,
        Err(e) => return Confinement::Unavailable(format!("can't find the tern binary: {e}")),
    };
    let out = std::process::Command::new(&helper)
        .args([HELPER_ARG, "--probe"])
        .stdin(Stdio::null())
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) => return Confinement::Unavailable(format!("running {}: {e}", helper.display())),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    // "landlock <abi> <status>"
    let mut words = text.split_whitespace();
    match (
        words.next(),
        words.next().and_then(|a| a.parse::<u8>().ok()),
    ) {
        (Some("landlock"), Some(abi)) if out.status.success() && abi > 0 => {
            Confinement::Landlock { abi }
        }
        (Some("landlock"), Some(_)) => Confinement::Unavailable(
            "the kernel has no Landlock (needs Linux 5.13+ with landlock in the lsm= boot list)"
                .into(),
        ),
        _ => Confinement::Unavailable(format!(
            "the sandbox helper {} didn't answer: {}",
            helper.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn detect_os(_policy: &HostPolicy) -> Confinement {
    Confinement::Unavailable("no OS sandbox on this platform".into())
}

#[async_trait]
impl SandboxBackend for HostBackend {
    fn verify(&self, workspace: &Path) -> Verification {
        HostBackend::verify(self, workspace)
    }

    fn verify_now(&self, workspace: &Path) -> Verification {
        HostBackend::verify_now(self, workspace)
    }

    fn warm(&self, workspace: &Path) {
        HostBackend::warm(self, workspace);
    }

    fn probe_blocking(
        &self,
        program: Vec<std::ffi::OsString>,
        cwd: &Path,
    ) -> Result<String, String> {
        self.probe_confined(program, cwd)
    }

    fn writable_roots(&self, workspace: &Path) -> Vec<PathBuf> {
        policy::writable_roots(&self.policy, workspace)
    }

    fn unconfined(&self) -> bool {
        matches!(
            self.confinement(),
            Confinement::Off | Confinement::Unavailable(_)
        )
    }

    /// `network = false` only holds where an OS sandbox enforces it.
    fn network_reachable(&self) -> bool {
        self.policy.network
            || matches!(
                self.confinement(),
                Confinement::Off | Confinement::Unavailable(_)
            )
    }

    async fn exec(
        &self,
        cmd: &str,
        opts: ExecOpts,
        cancel: CancellationToken,
    ) -> Result<ExecOutput, SandboxError> {
        let argv = self.argv(cmd, &opts.cwd, opts.network, &opts.read_roots)?;
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| SandboxError::Spawn(std::io::Error::other("empty command")))?;
        let mut command = tokio::process::Command::new(program);
        command.args(args);
        command.current_dir(&opts.cwd).env_clear().envs(
            PASSTHROUGH_ENV
                .iter()
                .filter_map(|k| std::env::var_os(k).map(|v| (k, v))),
        );
        command.envs(opts.env.iter().map(|(k, v)| (k, v)));
        run_captured(command, &opts, &cancel, || async {}).await
    }
}

/// Spawn `command`, capture its output, and enforce the timeout and
/// cancellation. `on_stop` runs after the child is killed (the docker
/// backend uses it to stop the process inside the container).
pub(crate) async fn run_captured<F, Fut>(
    mut command: tokio::process::Command,
    opts: &ExecOpts,
    cancel: &CancellationToken,
    on_stop: F,
) -> Result<ExecOutput, SandboxError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Its own process group, so a timeout or cancel can kill everything the
    // command started, not just its shell.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(SandboxError::Spawn)?;
    let pgid = child.id();

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let max = opts.max_output;
    let (tx, forwarder) = match &opts.on_output {
        Some(sink) => {
            let (tx, rx) = mpsc::unbounded_channel();
            (Some(tx), Some(tokio::spawn(forward(rx, sink.clone()))))
        }
        None => (None, None),
    };
    let mut out_task = tokio::spawn(read_capped(stdout, max, tx.clone()));
    let mut err_task = tokio::spawn(read_capped(stderr, max, tx));

    let mut result = ExecOutput::default();
    tokio::select! {
        status = child.wait() => {
            result.exit_code = status.map_err(SandboxError::Wait)?.code();
        }
        () = tokio::time::sleep(opts.timeout) => {
            result.timed_out = true;
            kill_group(pgid);
            let _ = child.kill().await;
        }
        () = cancel.cancelled() => {
            result.cancelled = true;
            kill_group(pgid);
            let _ = child.kill().await;
        }
    }
    if result.timed_out || result.cancelled {
        on_stop().await;
    }
    // Background processes may hold the pipes open; don't wait forever.
    let grace = Duration::from_secs(2);
    let (out, out_trunc) = tokio::time::timeout(grace, &mut out_task)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let (err, err_trunc) = tokio::time::timeout(grace, &mut err_task)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    // Drop the readers' senders so the forwarder flushes and ends.
    out_task.abort();
    err_task.abort();
    if let Some(f) = forwarder {
        let _ = tokio::time::timeout(grace, f).await;
    }
    result.stdout = out;
    result.stderr = err;
    result.truncated = out_trunc || err_trunc;
    Ok(result)
}

/// SIGKILL the process group led by `pgid` (the command's shell).
#[cfg(unix)]
fn kill_group(pgid: Option<u32>) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    if let Some(pid) = pgid.and_then(|p| i32::try_from(p).ok()) {
        let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_group(_pgid: Option<u32>) {}

/// Batch chunks from the readers and hand them to `sink` at most once per
/// `STREAM_INTERVAL`. Ends (after a final flush) when every sender is gone.
async fn forward(mut rx: mpsc::UnboundedReceiver<String>, sink: OutputSink) {
    while let Some(first) = rx.recv().await {
        let mut batch = first;
        let deadline = tokio::time::Instant::now() + STREAM_INTERVAL;
        loop {
            tokio::select! {
                more = rx.recv() => match more {
                    Some(s) => batch.push_str(&s),
                    None => break,
                },
                () = tokio::time::sleep_until(deadline) => break,
            }
        }
        (sink.0)(&batch);
    }
}

/// Split off the longest prefix of `bytes` that is complete UTF-8 (invalid
/// bytes become U+FFFD); a partial character at the end is left in `bytes`.
fn take_text(bytes: &mut Vec<u8>) -> String {
    let mut text = String::new();
    loop {
        match std::str::from_utf8(bytes) {
            Ok(s) => {
                text.push_str(s);
                bytes.clear();
                return text;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                text.push_str(&String::from_utf8_lossy(&bytes[..valid]));
                match e.error_len() {
                    None => {
                        bytes.drain(..valid);
                        return text;
                    }
                    Some(n) => {
                        text.push('\u{FFFD}');
                        bytes.drain(..valid + n);
                    }
                }
            }
        }
    }
}

/// Keeps the first quarter and the last three quarters of a stream's
/// bytes, counting what falls between.
struct HeadTail {
    head: Vec<u8>,
    head_cap: usize,
    tail: std::collections::VecDeque<u8>,
    tail_cap: usize,
    omitted: usize,
}

impl HeadTail {
    fn new(max: usize) -> Self {
        let head_cap = max / 4;
        Self {
            head: Vec::new(),
            head_cap,
            tail: std::collections::VecDeque::new(),
            tail_cap: max - head_cap,
            omitted: 0,
        }
    }

    fn push(&mut self, mut data: &[u8]) {
        let room = self.head_cap.saturating_sub(self.head.len());
        let n = room.min(data.len());
        self.head.extend_from_slice(&data[..n]);
        data = &data[n..];
        if data.len() >= self.tail_cap {
            self.omitted += self.tail.len() + data.len() - self.tail_cap;
            self.tail.clear();
            data = &data[data.len() - self.tail_cap..];
        } else {
            let over = (self.tail.len() + data.len()).saturating_sub(self.tail_cap);
            self.tail.drain(..over);
            self.omitted += over;
        }
        self.tail.extend(data.iter().copied());
    }

    fn finish(self) -> (String, bool) {
        let mut out = String::from_utf8_lossy(&self.head).into_owned();
        if self.omitted > 0 {
            out.push_str(&format!("\n[... {} bytes omitted ...]\n", self.omitted));
        }
        let tail: Vec<u8> = self.tail.into_iter().collect();
        out.push_str(&String::from_utf8_lossy(&tail));
        (out, self.omitted > 0)
    }
}

/// Read everything, keeping the head and tail within `max` bytes (and
/// streaming the first `max` bytes to `live`). Returns (text, truncated).
async fn read_capped<R: AsyncRead + Unpin>(
    reader: Option<R>,
    max: usize,
    live: Option<mpsc::UnboundedSender<String>>,
) -> (String, bool) {
    let Some(mut reader) = reader else {
        return (String::new(), false);
    };
    let mut kept = HeadTail::new(max);
    let mut seen = 0usize;
    let mut pending = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                kept.push(&buf[..n]);
                if let Some(tx) = &live {
                    let take = n.min(max.saturating_sub(seen));
                    pending.extend_from_slice(&buf[..take]);
                    let text = take_text(&mut pending);
                    if !text.is_empty() {
                        let _ = tx.send(text);
                    }
                }
                seen += n;
            }
        }
    }
    if let Some(tx) = &live
        && !pending.is_empty()
    {
        let _ = tx.send(String::from_utf8_lossy(&pending).into_owned());
    }
    kept.finish()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn opts(dir: &std::path::Path) -> ExecOpts {
        ExecOpts {
            cwd: dir.to_path_buf(),
            timeout: Duration::from_secs(10),
            max_output: 1024,
            on_output: None,
            network: false,
            env: Vec::new(),
            read_roots: Vec::new(),
        }
    }

    #[test]
    fn confine_argv_wraps_any_program() {
        let dir = tempfile::tempdir().unwrap();
        let b = HostBackend::new(HostPolicy {
            network: false,
            ..HostPolicy::confined()
        });
        let program: Vec<std::ffi::OsString> = vec!["/bin/tern".into(), "__monty-exec".into()];
        let argv = b.confine_argv(program.clone(), dir.path()).unwrap();
        assert_eq!(argv[argv.len() - 2..], program[..], "{argv:?}");
        match b.confinement() {
            #[cfg(target_os = "macos")]
            Confinement::Seatbelt => {
                assert_eq!(argv[0], SANDBOX_EXEC);
                let profile = argv[2].to_string_lossy();
                assert!(
                    profile.contains("(deny network-outbound (remote ip))"),
                    "{profile}"
                );
            }
            #[cfg(target_os = "linux")]
            Confinement::Landlock { .. } => {
                assert_eq!(argv[1], HELPER_ARG);
                assert!(argv.iter().any(|a| a == "--no-network"), "{argv:?}");
            }
            // No OS sandbox here: the program runs as given.
            _ => assert_eq!(argv, program),
        }
    }

    #[tokio::test]
    async fn runs_in_cwd_and_captures_output() {
        let tmp = tempfile::tempdir().unwrap();
        let out = HostBackend::default()
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
        let out = HostBackend::default()
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
        let out = HostBackend::default()
            .exec("sleep 5", o, CancellationToken::new())
            .await
            .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
    }

    #[tokio::test]
    async fn timeout_kills_background_children() {
        let tmp = tempfile::tempdir().unwrap();
        let mut o = opts(tmp.path());
        o.timeout = Duration::from_millis(500);
        let out = HostBackend::default()
            .exec(
                "sleep 30 & echo $! > child.pid; wait",
                o,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(out.timed_out);
        let pid: i32 = std::fs::read_to_string(tmp.path().join("child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Killed children linger as zombies until init reaps them.
        let mut alive = true;
        for _ in 0..50 {
            alive = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok();
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!alive, "background child {pid} outlived the timeout");
    }

    #[tokio::test]
    async fn required_confinement_refuses_when_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let backend = HostBackend::new(HostPolicy {
            confine: Confine::Required,
            helper: Some(tmp.path().join("no-such-helper")),
            ..HostPolicy::confined()
        });
        if *backend.confinement() == Confinement::Seatbelt {
            return; // macOS doesn't use the helper.
        }
        let err = backend
            .exec("true", opts(tmp.path()), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::Unconfined(_)), "{err}");
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
        let out = HostBackend::default()
            .exec("sleep 5", opts(tmp.path()), cancel)
            .await
            .unwrap();
        assert!(out.cancelled);
    }

    #[tokio::test]
    async fn caps_output() {
        let tmp = tempfile::tempdir().unwrap();
        let out = HostBackend::default()
            .exec(
                "head -c 5000 /dev/zero | tr '\\0' a",
                opts(tmp.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(out.stdout.len() < 1024 + 64);
        assert!(out.truncated);
        assert!(out.stdout.contains("bytes omitted"));
    }

    #[tokio::test]
    async fn keeps_the_tail_of_long_output() {
        let tmp = tempfile::tempdir().unwrap();
        for stream in ["", ">&2"] {
            let out = HostBackend::default()
                .exec(
                    &format!("seq 1 5000 {stream}; echo FINAL SUMMARY {stream}"),
                    opts(tmp.path()),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            let text = if stream.is_empty() {
                &out.stdout
            } else {
                &out.stderr
            };
            assert!(out.truncated);
            assert!(text.starts_with("1\n2\n"), "{text}");
            assert!(text.contains("bytes omitted"), "{text}");
            assert!(text.trim_end().ends_with("FINAL SUMMARY"), "{text}");
            assert!(text.len() < 1024 + 64);
        }
    }

    #[test]
    fn head_tail_accounts_for_every_byte() {
        let mut h = HeadTail::new(100);
        for chunk in (0..1000u32).map(|i| vec![b'a' + (i % 26) as u8; 7]) {
            h.push(&chunk);
        }
        let (text, trunc) = h.finish();
        assert!(trunc);
        let marker = text.find("\n[... ").unwrap();
        assert_eq!(marker, 25);
        let n: usize = text[marker + 6..]
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(n, 7000 - 100);
        let mut small = HeadTail::new(100);
        small.push(b"hello");
        assert_eq!(small.finish(), ("hello".to_owned(), false));
    }
}
