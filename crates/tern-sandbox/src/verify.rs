//! Is the OS sandbox actually active? A canary that runs a tiny command
//! through the same wrapper production commands use (`HostBackend::wrap`)
//! and checks what the sandbox blocks. See `docs/sandbox-design.md`.
//!
//! "Detected" (`sandbox-exec` exists, the helper answered) is not "active":
//! a nested sandbox, a profile the OS rejects, or a filter that doesn't
//! bite all look fine to `detect`. Anything that waives an approval on the
//! strength of the sandbox (a shell script grant) asks this instead.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::{Confinement, HostBackend, PASSTHROUGH_ENV, policy};

/// What a canary found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    /// Writes outside the workspace failed (and one inside worked), and, when
    /// the policy has the network off, an outbound connect failed. The text
    /// says what confines commands ("macOS Seatbelt").
    Verified(String),
    /// Not confirmed, and why, in plain words.
    NotVerified(String),
}

impl Verification {
    pub fn is_verified(&self) -> bool {
        matches!(self, Self::Verified(_))
    }

    pub fn not_verified(why: impl Into<String>) -> Self {
        Self::NotVerified(why.into())
    }

    /// The reason, when not verified.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::NotVerified(w) => Some(w),
            Self::Verified(_) => None,
        }
    }

    /// One line for the ledger, `tern doctor` and the API.
    pub fn describe(&self) -> String {
        match self {
            Self::Verified(how) => format!("verified: {how}"),
            Self::NotVerified(why) => format!("not verified: {why}"),
        }
    }
}

const VERIFIED_FOR: Duration = Duration::from_secs(600);
const NOT_VERIFIED_FOR: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// What the cache holds for one (policy, workspace).
enum Slot {
    Done(Instant, Verification),
    /// A probe started at this time is running on its own thread.
    Probing(Instant),
}

/// Results by (policy, workspace). The key holds everything the canary's
/// outcome depends on, so a config change (confine mode, network, paths,
/// helper) probes again; the time limits cover the environment changing
/// under a running daemon. Shared by every backend with the same policy, so
/// `tern doctor` and the daemon don't each probe. The lock is only ever held
/// to read or write an entry, never across a probe.
static CACHE: Mutex<Option<HashMap<String, Slot>>> = Mutex::new(None);

/// A probe that has run this long is taken to have died.
const PROBING_STALE: Duration = Duration::from_secs(60);

/// What the non-blocking [`HostBackend::verify`] says until a probe lands.
pub const CHECKING: &str = "still checking the sandbox; ask again shortly";

impl HostBackend {
    fn cache_key(&self, workspace: &Path) -> String {
        format!("{:?}|{}", self.policy, policy::canonical(workspace).display())
    }

    fn fresh(slot: &Slot) -> Option<Verification> {
        match slot {
            Slot::Done(at, v) => {
                let limit = if v.is_verified() {
                    VERIFIED_FOR
                } else {
                    NOT_VERIFIED_FOR
                };
                (at.elapsed() < limit).then(|| v.clone())
            }
            Slot::Probing(_) => None,
        }
    }

    /// Whether the OS sandbox is confirmed active for commands run in
    /// `workspace`, **without ever probing on the caller's thread**: it reads
    /// the cache (10 minutes; 30 seconds after a failure). With nothing fresh
    /// it starts a probe on a background thread and answers "not verified:
    /// still checking" (not cached) until that lands, so the agent loop never
    /// waits on a canary. Call [`HostBackend::warm`] at startup so the first
    /// call finds an answer; use [`HostBackend::verify_now`] where blocking
    /// is fine (`tern doctor`, tests).
    pub fn verify(&self, workspace: &Path) -> Verification {
        let key = self.cache_key(workspace);
        {
            let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
            let cache = guard.get_or_insert_with(HashMap::new);
            if let Some(slot) = cache.get(&key) {
                if let Some(v) = Self::fresh(slot) {
                    return v;
                }
                if let Slot::Probing(at) = slot
                    && at.elapsed() < PROBING_STALE
                {
                    return Verification::not_verified(CHECKING);
                }
            }
            cache.insert(key.clone(), Slot::Probing(Instant::now()));
        }
        let policy = self.policy.clone();
        let workspace = workspace.to_path_buf();
        let spawned = std::thread::Builder::new()
            .name("tern-sandbox-canary".into())
            .spawn(move || {
                let v = HostBackend::new(policy).run_canary(&workspace);
                let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
                guard
                    .get_or_insert_with(HashMap::new)
                    .insert(key, Slot::Done(Instant::now(), v));
            });
        if spawned.is_err() {
            return Verification::not_verified("couldn't start the sandbox check");
        }
        Verification::not_verified(CHECKING)
    }

    /// Start checking in the background if nothing fresh is cached.
    pub fn warm(&self, workspace: &Path) {
        let _ = self.verify(workspace);
    }

    /// The answer, probing on this thread if the cache has nothing fresh
    /// (blocking: up to a few seconds). The lock is not held while probing.
    pub fn verify_now(&self, workspace: &Path) -> Verification {
        let key = self.cache_key(workspace);
        {
            let guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(v) = guard.as_ref().and_then(|c| c.get(&key)).and_then(Self::fresh) {
                return v;
            }
        }
        self.verify_fresh(workspace)
    }

    /// Probe now on this thread, ignoring and replacing the cache.
    pub fn verify_fresh(&self, workspace: &Path) -> Verification {
        let key = self.cache_key(workspace);
        let v = self.run_canary(workspace);
        let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get_or_insert_with(HashMap::new)
            .insert(key, Slot::Done(Instant::now(), v.clone()));
        v
    }

    fn run_canary(&self, workspace: &Path) -> Verification {
        let how = match self.confinement() {
            Confinement::Off => {
                return Verification::not_verified("sandbox.confine is \"off\"");
            }
            Confinement::Unavailable(why) => {
                return Verification::not_verified(format!("no OS sandbox here: {why}"));
            }
            c => c.to_string(),
        };
        let workspace = policy::canonical(workspace);
        if !workspace.is_dir() {
            return Verification::not_verified("the workspace folder doesn't exist yet");
        }
        let Some(outside) = self.outside_target(&workspace) else {
            return Verification::not_verified(
                "no folder outside the sandbox's writable areas to probe",
            );
        };
        let inside = workspace.join(format!(
            ".tern-canary-inside-{}",
            outside.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned())
        ));
        let result = self.fs_probe(&workspace, &inside, &outside);
        // If the sandbox failed to block, this is the file it let through.
        let leaked = outside.exists();
        let inside_ok = inside.exists();
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_file(&inside);
        match result {
            Err(why) => return Verification::not_verified(why),
            Ok(()) if leaked => {
                return Verification::not_verified(
                    "a write outside the workspace was not blocked",
                );
            }
            Ok(()) => {}
        }
        if !inside_ok {
            return Verification::not_verified("the probe's write inside the workspace is missing");
        }
        if !self.policy.network {
            if let Err(why) = self.network_probe(&workspace) {
                return Verification::not_verified(why);
            }
            return Verification::Verified(format!(
                "{how}: writes outside the workspace and outbound connections were refused"
            ));
        }
        Verification::Verified(format!(
            "{how}: writes outside the workspace were refused (network is on)"
        ))
    }

    /// A path no sandboxed command may create, and that we can clean up:
    /// in `$HOME` or the workspace's parent, whichever isn't writable.
    fn outside_target(&self, workspace: &Path) -> Option<PathBuf> {
        let writable: Vec<PathBuf> = policy::writable_roots(&self.policy, workspace);
        let name = format!(
            ".tern-canary-outside-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(h) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            candidates.push(policy::canonical(Path::new(&h)));
        }
        if let Some(p) = workspace.parent() {
            candidates.push(p.to_path_buf());
        }
        candidates
            .into_iter()
            .filter(|d| d.is_dir())
            .find(|d| !writable.iter().any(|w| d.starts_with(w)))
            .map(|d| d.join(name))
    }

    fn fs_probe(&self, workspace: &Path, inside: &Path, outside: &Path) -> Result<(), String> {
        // The positive control (a write inside works) tells "the sandbox
        // blocked it" from "nothing ran": a nested sandbox fails to start.
        let script = "echo ok > \"$1\" || exit 10; if ( echo x > \"$2\" ) 2>/dev/null; then exit 11; fi; exit 0";
        let shell: Vec<OsString> = vec![
            "sh".into(),
            "-c".into(),
            script.into(),
            "sh".into(),
            inside.into(),
            outside.into(),
        ];
        let (code, stderr) = self.run_probe(shell, workspace, false)?;
        match code {
            Some(0) => Ok(()),
            Some(11) => Err("a write outside the workspace was not blocked".into()),
            Some(c) => Err(format!(
                "the sandbox didn't run a probe command (exit {c}){}: it may be nested inside another sandbox",
                first_line(&stderr)
            )),
            None => Err("the probe command was killed".into()),
        }
    }

    /// An outbound connect must fail. A listener on loopback is the oracle:
    /// if it accepts anything, the sandbox let the connection through. The
    /// probe tool must work unsandboxed (control) and must report that it
    /// tried and was refused (42), not merely fail to start.
    fn network_probe(&self, workspace: &Path) -> Result<(), String> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| format!("couldn't open a probe listener: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("probe listener: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("probe listener: {e}"))?
            .port()
            .to_string();
        let tools: [(&str, &[&str]); 2] = [
            (
                "perl",
                &[
                    "-e",
                    "use IO::Socket::INET; my $s = IO::Socket::INET->new(PeerAddr => \"127.0.0.1:$ARGV[0]\", Timeout => 3); exit($s ? 0 : 42);",
                ],
            ),
            (
                "python3",
                &[
                    "-c",
                    "import socket,sys\ntry:\n    socket.create_connection(('127.0.0.1', int(sys.argv[1])), 3)\nexcept OSError:\n    sys.exit(42)\n",
                ],
            ),
        ];
        let mut last = "neither perl nor python3 is available to probe the network block".to_owned();
        for (tool, args) in tools {
            let mut argv: Vec<OsString> = vec![tool.into()];
            argv.extend(args.iter().map(OsString::from));
            argv.push(port.clone().into());
            // Control: unsandboxed, the tool connects.
            let control = std::process::Command::new(tool)
                .args(args)
                .arg(&port)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if !control.is_ok_and(|s| s.success()) {
                continue;
            }
            drain(&listener);
            let (code, _) = self.run_probe(argv, workspace, false)?;
            std::thread::sleep(Duration::from_millis(50));
            if drain(&listener) {
                return Err("an outbound connection was not blocked".into());
            }
            match code {
                Some(42) => return Ok(()),
                Some(0) => return Err("an outbound connection was not blocked".into()),
                other => {
                    last = format!("the network probe ({tool}) didn't run to completion ({other:?})");
                }
            }
        }
        Err(last)
    }

    /// Run `program` under the production wrapper and wait (bounded).
    fn run_probe(
        &self,
        program: Vec<OsString>,
        cwd: &Path,
        network: bool,
    ) -> Result<(Option<i32>, String), String> {
        let policy = if network && !self.policy.network {
            crate::HostPolicy {
                network: true,
                ..self.policy.clone()
            }
        } else {
            self.policy.clone()
        };
        let argv = self.wrap(&policy, program, cwd).map_err(|e| e.to_string())?;
        let (prog, args) = argv.split_first().ok_or("empty probe command")?;
        let mut child = std::process::Command::new(prog)
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(
                PASSTHROUGH_ENV
                    .iter()
                    .filter_map(|k| std::env::var_os(k).map(|v| (k, v))),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("couldn't start the probe: {e}"))?;
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let mut err = String::new();
                    if let Some(mut s) = child.stderr.take() {
                        use std::io::Read;
                        let _ = s.read_to_string(&mut err);
                    }
                    return Ok((status.code(), err));
                }
                Ok(None) if start.elapsed() > PROBE_TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("the probe command timed out".into());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => return Err(format!("waiting for the probe: {e}")),
            }
        }
    }
}

/// Accept everything queued on `l`; whether anything was.
fn drain(l: &std::net::TcpListener) -> bool {
    let mut any = false;
    while l.accept().is_ok() {
        any = true;
    }
    any
}

fn first_line(s: &str) -> String {
    match s.lines().find(|l| !l.trim().is_empty()) {
        Some(l) => format!(": {}", l.trim()),
        None => String::new(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{Confine, HostPolicy};

    #[test]
    fn no_confinement_is_never_verified() {
        let ws = tempfile::tempdir().unwrap();
        let b = HostBackend::unconfined();
        let v = b.verify_fresh(ws.path());
        assert!(!v.is_verified(), "{v:?}");
        assert!(v.reason().unwrap().contains("off"), "{v:?}");
        assert!(v.describe().starts_with("not verified"));
    }

    #[test]
    fn a_missing_workspace_is_not_verified() {
        let ws = tempfile::tempdir().unwrap();
        let b = HostBackend::new(HostPolicy {
            confine: Confine::Auto,
            ..HostPolicy::default()
        });
        let v = b.verify_fresh(&ws.path().join("nope"));
        assert!(!v.is_verified(), "{v:?}");
    }

    #[test]
    fn a_probe_leaves_no_files_and_its_answer_is_cached() {
        let ws = tempfile::tempdir().unwrap();
        let b = HostBackend::default();
        let first = b.verify_fresh(ws.path());
        // Whatever the answer on this machine (a nested sandbox can't start
        // another, and says so), nothing is left in the workspace.
        let left: Vec<_> = std::fs::read_dir(ws.path()).unwrap().collect();
        assert!(left.is_empty(), "{left:?}");
        // Cached: the same answer without probing again.
        assert_eq!(b.verify(ws.path()), first);
    }

    #[test]
    fn the_non_blocking_check_never_probes_on_the_callers_thread() {
        let ws = tempfile::tempdir().unwrap();
        let b = HostBackend::default();
        // Nothing cached for this workspace: an immediate "still checking",
        // not a probe, and not remembered as an answer.
        let first = b.verify(ws.path());
        assert_eq!(first, Verification::not_verified(CHECKING));
        // A caller holding the cache lock is never needed to answer: while a
        // probe is in flight, other callers get the same line at once.
        // The background probe lands, and agrees with a blocking one.
        let end = Instant::now() + Duration::from_secs(60);
        let landed = loop {
            let v = b.verify(ws.path());
            if v.reason() != Some(CHECKING) {
                break v;
            }
            assert!(Instant::now() < end, "the background probe never landed");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(landed, b.verify_now(ws.path()));
        // And it left nothing behind.
        assert_eq!(std::fs::read_dir(ws.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_blocking_check_does_not_hold_the_cache_while_probing() {
        // Another thread can read and write the cache while a probe runs:
        // start a blocking probe and take the lock from here meanwhile.
        let ws = tempfile::tempdir().unwrap();
        let path = ws.path().to_path_buf();
        let probe = std::thread::spawn(move || HostBackend::default().verify_fresh(&path));
        for _ in 0..50 {
            let t = Instant::now();
            drop(CACHE.lock().unwrap_or_else(|e| e.into_inner()));
            assert!(t.elapsed() < Duration::from_millis(500));
            std::thread::sleep(Duration::from_millis(5));
        }
        probe.join().unwrap();
    }

    #[test]
    fn the_outside_target_is_never_a_writable_place() {
        let ws = tempfile::tempdir().unwrap();
        let ws = std::fs::canonicalize(ws.path()).unwrap();
        let b = HostBackend::default();
        if let Some(t) = b.outside_target(&ws) {
            let roots = policy::writable_roots(&b.policy, &ws);
            assert!(
                !roots.iter().any(|r| t.starts_with(r)),
                "{t:?} in {roots:?}"
            );
        }
    }
}
