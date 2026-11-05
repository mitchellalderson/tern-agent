//! The `shell` tool, run through a `SandboxBackend`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::tool::{
    Effect, GrantKey, Tool, ToolCtx, ToolPolicy, ToolResult, current_call_id, planning_scope,
    progress_sink,
};
use tern_core::types::ToolSpec;
use tern_sandbox::{ExecOpts, OutputSink, SandboxBackend};

use crate::shell_script::{self, Prober, Resolved};
use crate::{args, spec};

pub struct Shell {
    backend: Arc<dyn SandboxBackend>,
    default_timeout: Duration,
    /// What `policy` decided for each call that may be covered by a script
    /// grant, by (run id, call id). `call` uses up exactly its own.
    plans: Mutex<HashMap<String, Entry>>,
    /// The `PATH` interpreters are looked up on (the process's own).
    path: Option<OsString>,
    /// Tells this tool's inspection results from another's.
    id: u64,
}

/// What `policy` saw of one call. Kept per call so a grant is offered for,
/// and `call` runs, exactly the script that was hashed then.
#[derive(Clone)]
struct Entry {
    /// The command text the plan is for.
    command: String,
    /// Set for a `python3 file.py args` call that resolved: the snapshot
    /// `call` will run and re-check.
    plan: Option<Resolved>,
    /// Offered only when the sandbox is confirmed active and offline.
    key: Option<GrantKey>,
    /// Why a python command isn't auto-approvable, for the card.
    notice: Option<String>,
    /// A scoped grant decided this call (nobody was asked): it must run
    /// exactly its plan or not at all, never the typed command.
    granted: bool,
    /// The grant key that decided it (`granted`), which a re-plan must still
    /// produce for the call to run.
    approved_key: Option<String>,
}

/// Inspects interpreters inside the sandbox, never outside it.
struct Confined<'a>(&'a dyn SandboxBackend, u64);

impl Prober for Confined<'_> {
    fn cache_scope(&self) -> String {
        format!("shell-{}", self.1)
    }
    fn inspect(&self, program: Vec<OsString>, cwd: &std::path::Path) -> Result<String, String> {
        self.0.probe_blocking(program, cwd)
    }
}

fn plan_key(run: &str, call: &str) -> String {
    format!("{run}\0{call}")
}

impl Shell {
    pub fn new(backend: Arc<dyn SandboxBackend>, default_timeout: Duration) -> Self {
        Self {
            backend,
            default_timeout,
            plans: Mutex::new(HashMap::new()),
            path: std::env::var_os("PATH"),
            id: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
        }
    }

    /// Look interpreters up on `path` instead of the process's `PATH`.
    pub fn with_path(mut self, path: Option<OsString>) -> Self {
        self.path = path;
        self
    }

    /// How many plans are held (tests).
    pub fn plans_held(&self) -> usize {
        self.plans.lock().map_or(0, |m| m.len())
    }

    fn planning_id() -> String {
        let (run, call) = planning_scope().unwrap_or_default();
        plan_key(&run, &call)
    }

    fn take_entry(&self, ctx: &ToolCtx) -> Option<Entry> {
        let id = plan_key(&ctx.run_id, &current_call_id().unwrap_or_default());
        self.plans.lock().ok().and_then(|mut m| m.remove(&id))
    }

    /// Judge one call: is it a covered script run, and may a grant be
    /// offered for it? Never fails the call: anything unusual is just "not
    /// covered" and the command asks as before.
    fn judge(&self, ctx: &ToolCtx, args: &Value) -> Entry {
        let command = args["command"].as_str().unwrap_or_default().trim().to_owned();
        let none = |notice: Option<String>| Entry {
            command: command.clone(),
            plan: None,
            key: None,
            notice,
            granted: false,
            approved_key: None,
        };
        let parsed = match shell_script::parse(&command) {
            Ok(p) => p,
            Err(rej) => return none(rej.notice()),
        };
        if !shell_script::offline(args) {
            return none(Some(
                "Not auto-approved: the command asked for network access.".into(),
            ));
        }
        let writable = self.backend.writable_roots(&ctx.workspace);
        let verification = self.backend.verify(&ctx.workspace);
        let resolved = match shell_script::resolve(
            &parsed,
            &ctx.workspace,
            &writable,
            self.path.clone(),
            &Confined(self.backend.as_ref(), self.id),
        ) {
            Ok(r) => r,
            Err(why) => {
                // Without a confirmed sandbox that is the reason to give.
                return none(Some(match verification.reason() {
                    Some(v) => format!("Not auto-approved: the sandbox isn't confirmed active ({v})."),
                    None => format!("Not auto-approved: {why}."),
                }));
            }
        };
        let why_not = if let Some(why) = verification.reason() {
            Some(format!(
                "Not auto-approved: the sandbox isn't confirmed active ({why})."
            ))
        } else if self.backend.network_reachable() {
            Some(
                "Not auto-approved: the sandbox allows network access, so a script could send data out."
                    .to_owned(),
            )
        } else {
            None
        };
        let key = why_not.is_none().then(|| grant_key(&resolved, &verification));
        Entry {
            command,
            plan: Some(resolved),
            key,
            notice: why_not,
            granted: false,
            approved_key: None,
        }
    }
}

/// What a script grant covers: this interpreter, this exact file (path and
/// bytes), these arguments, in this workspace, offline in a verified sandbox.
/// Anything else (an edited script, other arguments) is a different key.
fn grant_key(r: &Resolved, verification: &tern_sandbox::Verification) -> GrantKey {
    let material = json!({
        "v": 2,
        "interpreter": r.interpreter_canonical,
        "interpreter_as_found": r.interpreter,
        "script": r.script_canonical,
        "sha256": r.hash,
        "args": r.args,
        "workspace": r.workspace,
    });
    let digest = {
        use sha2::{Digest, Sha256};
        Sha256::digest(material.to_string().as_bytes())
            .iter()
            .take(12)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let short = &r.hash[..8];
    let args = if r.args.is_empty() {
        "no arguments".to_owned()
    } else {
        format!("arguments {}", r.args.join(" "))
    };
    GrantKey {
        key: format!("python script {}#{digest}", r.script_rel),
        label: format!("python3 {} (version {short}, {args})", r.script_rel),
        note: Some(
            "Covers this file as it is now, with these arguments, in the sandbox with no network. It runs isolated (python -I): the script must be self-contained, because modules next to it won't import, and standard-library names can't be shadowed. It can still change files in the workspace."
                .into(),
        ),
        audit: Some(
            json!({
                "script": r.script_rel,
                "script_path": r.script_canonical,
                "sha256": r.hash,
                "interpreter": r.interpreter_canonical,
                "args": r.args,
                "sandbox": verification.describe(),
            })
            .to_string(),
        ),
    }
}

#[derive(Deserialize)]
struct ShellArgs {
    command: String,
    timeout_secs: Option<u64>,
}

impl Shell {
    /// Whether this call asks for networking the sandbox doesn't give by
    /// default.
    fn wants_network(&self, args: &Value) -> bool {
        args["network"] == true && !self.backend.network_reachable()
    }
}

#[async_trait]
impl Tool for Shell {
    fn effect(&self) -> Effect {
        Effect::Destructive
    }

    /// With the network reachable a command can fetch anything (`curl`, a
    /// script calling an API), so its output taints the run like
    /// `http_fetch`'s. Without it, it only sees local files. A command that
    /// asked for network (`network: true`) marks its own result.
    fn untrusted(&self) -> bool {
        self.backend.network_reachable()
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "shell",
            "Run a shell command (sh -c) in the workspace directory. Returns the exit code, stdout, and stderr. The environment is minimal; no secrets are available. Commands have no network access unless you pass network: true, which the user must approve every time; use it only for things like git push/pull or installing packages, never to call a service's API (use its tool). To run Python, write the code to a .py file in the workspace (write_file) and run it as `python3 file.py args` with no pipes, redirects, quotes, `-c` or heredocs: output is already captured and capped, and that exact form can be approved once for the whole run. Batch the work into one script per question.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer", "minimum": 1 },
                    "network": {
                        "type": "boolean",
                        "description": "Give this command IP networking. Always asks the user."
                    }
                },
                "required": ["command"]
            }),
        )
    }

    fn policy(&self, ctx: &ToolCtx, args: &Value) -> ToolPolicy {
        // The call's own plan (by run and call id): the script, its hash and
        // whether a grant may be offered, decided once, here.
        let mut entry = self.judge(ctx, args);
        if let Ok(mut plans) = self.plans.lock() {
            let id = Self::planning_id();
            // A re-plan (an effect probe) must not forget that a grant
            // already decided this call.
            if let Some(old) = plans.get(&id).filter(|e| e.granted) {
                entry.granted = true;
                entry.approved_key = old.approved_key.clone();
            }
            if entry.plan.is_some() || entry.notice.is_some() || entry.granted {
                plans.insert(id, entry);
            } else {
                plans.remove(&id);
            }
        }
        // Destructive outside the allowlist, a granted write inside it
        // (`approval::check`). A call asking for network is never covered
        // by the allowlist (`approval::allowed_by`).
        ToolPolicy::new(Effect::Destructive, false).forced(true)
    }

    /// Only for `python3 file.py args` in a verified, offline sandbox, and
    /// only the key this very call planned (none without a plan).
    fn grant_key(&self, args: &Value) -> Option<GrantKey> {
        let command = args["command"].as_str()?.trim();
        let plans = self.plans.lock().ok()?;
        let entry = plans.get(&Self::planning_id())?;
        (entry.command == command).then(|| entry.key.clone()).flatten()
    }

    fn grant_unavailable(&self, args: &Value) -> Option<String> {
        let command = args["command"].as_str()?.trim();
        let plans = self.plans.lock().ok()?;
        let entry = plans.get(&Self::planning_id())?;
        (entry.command == command)
            .then(|| entry.notice.clone())
            .flatten()
    }

    fn grant_applied(&self, run_id: &str, call_id: &str) {
        if let Ok(mut m) = self.plans.lock() {
            let e = m
                .entry(plan_key(run_id, call_id))
                .or_insert_with(|| Entry {
                    command: String::new(),
                    plan: None,
                    key: None,
                    notice: None,
                    granted: false,
                    approved_key: None,
                });
            e.granted = true;
            e.approved_key = e.key.as_ref().map(|k| k.key.clone());
        }
    }

    fn forget_plans(&self, run_id: &str, call_id: Option<&str>) {
        if let Ok(mut m) = self.plans.lock() {
            match call_id {
                Some(c) => {
                    m.remove(&plan_key(run_id, c));
                }
                None => m.retain(|k, _| !k.starts_with(&format!("{run_id}\0"))),
            }
        }
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let cmd = args["command"].as_str()?;
        self.wants_network(args)
            .then(|| format!("with network: {cmd}"))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let network = self.wants_network(&raw);
        let a: ShellArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        if let Err(e) = tokio::fs::create_dir_all(&ctx.workspace).await {
            return ToolResult::error(format!("creating workspace: {e}"));
        }
        // A planned script run: the file must still be what was approved, and
        // what runs is the interpreter and file that were hashed.
        let mut command = a.command.clone();
        let entry = self.take_entry(ctx);
        // Fail closed: a grant covers one planned script, so a call it
        // decided that has no plan (or whose typed command isn't the planned
        // one) runs nothing, rather than falling through to `sh -c <typed>`.
        if let Some(e) = &entry
            && e.granted
            && (e.plan.is_none()
                || e.command != a.command.trim()
                || e.approved_key.is_none()
                || e.approved_key != e.key.as_ref().map(|k| k.key.clone()))
        {
            return ToolResult::error(
                "this call was covered by a grant, but what it would run can no longer be confirmed (the script or interpreter changed, or no longer qualifies); nothing was run. Make the call again and it will ask",
            );
        }
        if let Some(entry) = entry
            && let Some(plan) = entry.plan
        {
            if entry.command != a.command.trim() {
                return ToolResult::error(
                    "the command changed since its approval was decided; nothing was run",
                );
            }
            match shell_script::read_script(&plan.workspace, &plan.script_canonical) {
                Ok((path, hash)) if path == plan.script_canonical && hash == plan.hash => {}
                Ok(_) | Err(_) => {
                    return ToolResult::error(format!(
                        "{} changed since it was approved (its contents differ from version {}); nothing was run. Ask again to run the new version.",
                        plan.script_rel,
                        &plan.hash[..8]
                    ));
                }
            }
            let writable = self.backend.writable_roots(&ctx.workspace);
            let interpreter = plan
                .interpreter
                .file_name()
                .map_or_else(|| "python3".to_owned(), |n| n.to_string_lossy().into_owned());
            let again = shell_script::resolve(
                &shell_script::Parsed {
                    interpreter,
                    script: plan.script_rel.clone(),
                    args: plan.args.clone(),
                },
                &ctx.workspace,
                &writable,
                self.path.clone(),
                &Confined(self.backend.as_ref(), self.id),
            );
            if !again.is_ok_and(|r| {
                r.interpreter_canonical == plan.interpreter_canonical && r.hash == plan.hash
            }) {
                return ToolResult::error(format!(
                    "the interpreter or {} changed since it was approved; nothing was run",
                    plan.script_rel
                ));
            }
            command = shell_script::command_line(&plan);
        }
        let timeout = a
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);
        let opts = ExecOpts {
            cwd: ctx.workspace.clone(),
            timeout,
            max_output: 256 * 1024,
            on_output: progress_sink().map(|sink| OutputSink(Arc::new(move |s| sink(s)))),
            network,
        };
        let out = match self
            .backend
            .exec(&command, opts, ctx.cancel.clone())
            .await
        {
            Ok(o) => o,
            Err(e) => return ToolResult::error(e.to_string()),
        };
        let status = if out.timed_out {
            format!("timed out after {}s", timeout.as_secs())
        } else if out.cancelled {
            "cancelled".to_owned()
        } else {
            match out.exit_code {
                Some(c) => format!("exit code {c}"),
                None => "killed by signal".to_owned(),
            }
        };
        let mut text = status;
        if !out.stdout.is_empty() {
            text.push_str("\n--- stdout ---\n");
            text.push_str(&out.stdout);
        }
        if !out.stderr.is_empty() {
            text.push_str("\n--- stderr ---\n");
            text.push_str(&out.stderr);
        }
        if out.truncated {
            text.push_str("\n[output truncated]");
        }
        ToolResult {
            content: text,
            is_error: out.exit_code != Some(0),
            // What a command fetched is third-party text.
            untrusted: network,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;
    use std::sync::Mutex;

    use tern_core::tool::{ProgressSink, with_progress};
    use tern_sandbox::HostBackend;
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[tokio::test]
    async fn streams_output_while_running_and_still_returns_it() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            workspace: tmp.path().into(),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        let shell = Shell::new(Arc::new(HostBackend::default()), Duration::from_secs(10));
        let chunks = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink: ProgressSink = {
            let chunks = chunks.clone();
            Arc::new(move |s| chunks.lock().unwrap().push(s.to_owned()))
        };
        let r = with_progress(
            sink,
            shell.call(
                &ctx,
                json!({"command": "echo first; sleep 0.6; echo second >&2"}),
            ),
        )
        .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("first") && r.content.contains("second"));
        let chunks = chunks.lock().unwrap();
        assert_eq!(chunks.len(), 2, "{chunks:?}");
        assert_eq!(chunks[0], "first\n");
        assert_eq!(chunks[1], "second\n");
    }

    #[tokio::test]
    async fn a_command_given_network_says_so_and_its_output_is_untrusted() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ToolCtx {
            workspace: tmp.path().into(),
            run_id: "r".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        let offline = tern_sandbox::HostPolicy {
            network: false,
            ..tern_sandbox::HostPolicy::confined()
        };
        let shell = Shell::new(Arc::new(HostBackend::new(offline)), Duration::from_secs(10));
        let ask = json!({"command": "echo hi", "network": true});
        assert_eq!(
            shell.summary(&ask).as_deref(),
            Some("with network: echo hi")
        );
        assert!(shell.call(&ctx, ask).await.untrusted);
        let plain = json!({"command": "echo hi"});
        assert_eq!(shell.summary(&plain), None);
        assert!(!shell.call(&ctx, plain).await.untrusted);

        // With network on for every command, asking adds nothing: every
        // command's output is already untrusted (`Tool::untrusted`).
        let online = Shell::new(Arc::new(HostBackend::default()), Duration::from_secs(10));
        let ask = json!({"command": "echo hi", "network": true});
        assert_eq!(online.summary(&ask), None);
        assert!(online.untrusted());
    }

    #[test]
    fn output_is_untrusted_when_commands_can_reach_the_network() {
        let shell = |network| {
            let settings = tern_sandbox::DockerSettings {
                network,
                ..tern_sandbox::DockerSettings::default()
            };
            let backend = tern_sandbox::DockerBackend::new("docker", Path::new("/w"), settings);
            Shell::new(Arc::new(backend), Duration::from_secs(1))
        };
        assert!(shell(true).untrusted());
        assert!(!shell(false).untrusted());
        // No OS sandbox: `network = false` isn't enforced, so it counts as on.
        let mut policy = tern_sandbox::HostPolicy::unconfined();
        policy.network = false;
        let host = Shell::new(Arc::new(HostBackend::new(policy)), Duration::from_secs(1));
        assert!(host.untrusted());
    }
}

/// Script grants: the key, the sandbox gate, and the plan that makes
/// "approved" and "run" the same file.
#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod script_grant_tests {
    use std::path::{Path, PathBuf};

    use tern_core::tool::{ToolRegistry, with_call};
    use tern_core::types::ToolCall;
    use tern_sandbox::{ExecOutput, HostBackend, Verification};
    use tokio_util::sync::CancellationToken;

    use super::*;

    /// A backend that runs commands unconfined but claims whatever a test
    /// wants about its sandbox.
    struct Claim {
        inner: HostBackend,
        verification: Verification,
        network: bool,
        roots: Vec<PathBuf>,
    }

    impl Claim {
        fn new(verification: Verification, roots: Vec<PathBuf>) -> Arc<Self> {
            Arc::new(Self::plain(verification, roots))
        }

        fn plain(verification: Verification, roots: Vec<PathBuf>) -> Self {
            Self {
                inner: HostBackend::unconfined(),
                verification,
                network: false,
                roots,
            }
        }
    }

    #[async_trait]
    impl SandboxBackend for Claim {
        async fn exec(
            &self,
            cmd: &str,
            opts: ExecOpts,
            cancel: CancellationToken,
        ) -> Result<ExecOutput, tern_sandbox::SandboxError> {
            self.inner.exec(cmd, opts, cancel).await
        }
        fn network_reachable(&self) -> bool {
            self.network
        }
        fn verify(&self, _: &Path) -> Verification {
            self.verification.clone()
        }
        fn writable_roots(&self, _: &Path) -> Vec<PathBuf> {
            self.roots.clone()
        }
        /// Test double: runs the inspection directly (the real backend runs
        /// it inside the sandbox).
        fn probe_blocking(&self, program: Vec<OsString>, cwd: &Path) -> Result<String, String> {
            let (prog, args) = program.split_first().ok_or("empty")?;
            let out = std::process::Command::new(prog)
                .args(args)
                .current_dir(cwd)
                .env_clear()
                .output()
                .map_err(|e| e.to_string())?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
                .ok_or_else(|| "it didn't run".to_owned())
        }
    }

    /// Like docker: runs commands, can't be probed.
    struct NoProbe;

    #[async_trait]
    impl SandboxBackend for NoProbe {
        async fn exec(
            &self,
            _: &str,
            _: ExecOpts,
            _: CancellationToken,
        ) -> Result<ExecOutput, tern_sandbox::SandboxError> {
            Ok(ExecOutput::default())
        }
        fn network_reachable(&self) -> bool {
            false
        }
    }

    fn verified() -> Verification {
        Verification::Verified("test sandbox".into())
    }

    struct Env {
        _dir: tempfile::TempDir,
        ws: PathBuf,
        ctx: ToolCtx,
    }

    fn env() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let ws = std::fs::canonicalize(dir.path()).unwrap();
        let ctx = ToolCtx {
            workspace: ws.clone(),
            run_id: "run".into(),
            session_id: "s".into(),
            cancel: CancellationToken::new(),
        };
        Env { _dir: dir, ws, ctx }
    }

    fn registry(backend: Arc<dyn SandboxBackend>) -> (ToolRegistry, Arc<Shell>) {
        let shell = Arc::new(Shell::new(backend, Duration::from_secs(20)));
        let mut reg = ToolRegistry::new();
        reg.register(shell.clone());
        (reg, shell)
    }

    fn call(id: &str, command: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "shell".into(),
            input: json!({ "command": command }),
        }
    }

    /// Plan `c` as the loop does, and return its key.
    fn key(env: &Env, reg: &ToolRegistry, c: &ToolCall) -> Option<GrantKey> {
        reg.policy(&env.ctx, c);
        reg.grant_key(&env.ctx.run_id, c)
    }

    fn have_python() -> bool {
        std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    fn roots(e: &Env) -> Vec<PathBuf> {
        vec![e.ws.clone()]
    }

    /// A different native "python3" (a copy of the shell: a program, not a
    /// shim script), for keys that must differ by interpreter.
    fn fake_python(dir: &Path) {
        // Found at another path than the real one, so its key differs.
        let real = std::process::Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default();
        std::os::unix::fs::symlink(real, dir.join("python3")).unwrap();
    }

    #[test]
    fn the_key_names_script_bytes_arguments_interpreter_path_and_workspace() {
        let e = env();
        std::fs::write(e.ws.join("a.py"), "print(1)").unwrap();
        std::fs::write(e.ws.join("b.py"), "print(1)").unwrap();
        let (reg, _) = registry(Claim::new(verified(), roots(&e)));
        let k = |c: &str| key(&e, &reg, &call("1", c));
        let base = k("python3 a.py x").expect("offered");
        assert!(
            base.label.contains("a.py") && base.label.contains("arguments x"),
            "{}",
            base.label
        );
        assert!(
            base.note
                .as_deref()
                .unwrap()
                .contains("modules next to it won't import")
        );
        assert_eq!(k("python3 a.py x").unwrap(), base, "stable");
        assert_eq!(k("  python3   a.py   x ").unwrap(), base, "spacing");
        assert_ne!(k("python3 a.py y").unwrap().key, base.key, "args");
        assert_ne!(k("python3 a.py x y").unwrap().key, base.key, "more args");
        assert_ne!(k("python3 b.py x").unwrap().key, base.key, "path, same bytes");
        std::fs::write(e.ws.join("a.py"), "print(2)").unwrap();
        assert_ne!(k("python3 a.py x").unwrap().key, base.key, "bytes");
        std::fs::write(e.ws.join("a.py"), "print(1)").unwrap();
        assert_eq!(k("python3 a.py x").unwrap().key, base.key, "same bytes again");
        // Another interpreter file on PATH; another workspace.
        let bins = tempfile::tempdir().unwrap();
        fake_python(bins.path());
        let shell = Shell::new(Claim::new(verified(), roots(&e)), Duration::from_secs(5))
            .with_path(Some(bins.path().into()));
        let mut reg2 = ToolRegistry::new();
        reg2.register(Arc::new(shell));
        let other = key(&e, &reg2, &call("1", "python3 a.py x")).unwrap();
        assert_ne!(other.key, base.key, "interpreter");
        let e2 = env();
        std::fs::write(e2.ws.join("a.py"), "print(1)").unwrap();
        let (reg3, _) = registry(Claim::new(verified(), roots(&e2)));
        let in_other = key(&e2, &reg3, &call("1", "python3 a.py x")).unwrap();
        assert_ne!(in_other.key, base.key, "workspace");
        let audit: Value = serde_json::from_str(base.audit.as_deref().unwrap()).unwrap();
        assert_eq!(audit["sha256"].as_str().unwrap().len(), 64);
        assert!(audit["sandbox"].as_str().unwrap().starts_with("verified"));
        assert_eq!(audit["script"], "a.py");
    }

    #[test]
    fn only_the_covered_shape_gets_a_key_and_python_commands_say_why() {
        let e = env();
        std::fs::write(e.ws.join("a.py"), "print(1)").unwrap();
        let (reg, _) = registry(Claim::new(verified(), roots(&e)));
        for cmd in [
            "python3 -c 'print(1)'",
            "python3 a.py | head",
            "python3 a.py 2>&1",
            "python3 a.py; id",
            "python3 - <<'EOF'\nprint(1)\nEOF",
            "python3 -m http.server",
            "FOO=1 python3 a.py",
            "env python3 a.py",
            "python3 missing.py",
            "ls",
            "echo hi",
        ] {
            let c = call("1", cmd);
            assert_eq!(key(&e, &reg, &c), None, "{cmd:?}");
            let why = reg.grant_unavailable(&e.ctx.run_id, &c);
            let python_first = cmd.starts_with("python3");
            assert_eq!(why.is_some(), python_first, "{cmd:?}: {why:?}");
        }
        let c = call("1", "python3 -c 'x'");
        reg.policy(&e.ctx, &c);
        let why = reg.grant_unavailable("run", &c).unwrap();
        assert!(why.contains("inline script") && why.contains(".py file"), "{why}");
        let c = call("1", "python3 a.py | head -5");
        reg.policy(&e.ctx, &c);
        let why = reg.grant_unavailable("run", &c).unwrap();
        assert!(why.contains("pipe, redirect"), "{why}");
        // Network asked for: no key, even for a covered shape.
        let mut net = call("2", "python3 a.py");
        net.input["network"] = true.into();
        assert_eq!(key(&e, &reg, &net), None);
        assert!(reg.grant_unavailable("run", &net).unwrap().contains("network"));
        // A script that is a symlink to a file outside the workspace.
        let out = tempfile::tempdir().unwrap();
        std::fs::write(out.path().join("o.py"), "x").unwrap();
        std::os::unix::fs::symlink(out.path().join("o.py"), e.ws.join("o.py")).unwrap();
        let c = call("3", "python3 o.py");
        assert_eq!(key(&e, &reg, &c), None);
        let why = reg.grant_unavailable("run", &c).unwrap();
        assert!(why.contains("outside the workspace"), "{why}");
        // A call nothing planned has no key to match.
        assert_eq!(reg.grant_key("run", &call("never", "python3 a.py")), None);
    }

    #[test]
    fn no_key_unless_the_sandbox_is_verified_and_offline() {
        let e = env();
        std::fs::write(e.ws.join("a.py"), "print(1)").unwrap();
        let c = call("1", "python3 a.py");
        // A failing probe.
        let down = Verification::not_verified("it may be nested inside another sandbox");
        let (reg, _) = registry(Claim::new(down, roots(&e)));
        assert_eq!(key(&e, &reg, &c), None);
        let why = reg.grant_unavailable("run", &c).unwrap();
        assert!(
            why.starts_with("Not auto-approved: the sandbox isn't confirmed active"),
            "{why}"
        );
        assert!(why.contains("nested"), "{why}");
        // Verified: offered.
        let (reg, _) = registry(Claim::new(verified(), roots(&e)));
        assert!(key(&e, &reg, &c).is_some());
        assert_eq!(reg.grant_unavailable("run", &c), None);
        // Verified but the network is reachable: not offered.
        let mut online = Claim::plain(verified(), roots(&e));
        online.network = true;
        let (reg, _) = registry(Arc::new(online));
        assert_eq!(key(&e, &reg, &c), None);
        assert!(reg.grant_unavailable("run", &c).unwrap().contains("network access"));
        // A backend that can't be probed (docker) never verifies.
        let (reg, _) = registry(Arc::new(NoProbe));
        assert_eq!(key(&e, &reg, &c), None);
    }

    /// Verified and offline, but it can't inspect an interpreter inside the
    /// sandbox (the default for a backend that doesn't implement it).
    struct CannotInspect(Arc<Claim>);

    #[async_trait]
    impl SandboxBackend for CannotInspect {
        async fn exec(
            &self,
            cmd: &str,
            opts: ExecOpts,
            cancel: CancellationToken,
        ) -> Result<ExecOutput, tern_sandbox::SandboxError> {
            self.0.exec(cmd, opts, cancel).await
        }
        fn network_reachable(&self) -> bool {
            false
        }
        fn verify(&self, w: &Path) -> Verification {
            self.0.verify(w)
        }
        fn writable_roots(&self, w: &Path) -> Vec<PathBuf> {
            self.0.writable_roots(w)
        }
    }

    #[test]
    fn if_the_interpreter_cannot_be_inspected_in_the_sandbox_no_grant_is_offered() {
        if !have_python() {
            return;
        }
        let e = env();
        std::fs::write(e.ws.join("a.py"), "x").unwrap();
        let (reg, _) = registry(Arc::new(CannotInspect(Claim::new(verified(), roots(&e)))));
        let c = call("1", "python3 a.py");
        assert_eq!(key(&e, &reg, &c), None);
        let why = reg.grant_unavailable("run", &c).unwrap();
        assert!(why.contains("couldn't inspect"), "{why}");
    }

    #[test]
    fn an_interpreter_in_a_writable_place_is_never_covered() {
        let e = env();
        std::fs::write(e.ws.join("a.py"), "x").unwrap();
        let bin = e.ws.join("bin");
        std::fs::create_dir(&bin).unwrap();
        fake_python(&bin);
        let shell = Shell::new(Claim::new(verified(), roots(&e)), Duration::from_secs(5))
            .with_path(Some(bin.into()));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(shell));
        assert_eq!(key(&e, &reg, &call("1", "python3 a.py")), None);
    }

    #[tokio::test]
    async fn a_script_edited_between_approval_and_run_is_refused_and_nothing_runs() {
        if !have_python() {
            return;
        }
        let e = env();
        std::fs::write(e.ws.join("a.py"), "print('approved')").unwrap();
        let (reg, shell) = registry(Claim::new(verified(), roots(&e)));
        let c = call("1", "python3 a.py");
        let k = key(&e, &reg, &c).unwrap();
        // The agent rewrites the script while the card is open.
        std::fs::write(
            e.ws.join("a.py"),
            "open('pwned','w').write('x'); print('edited')",
        )
        .unwrap();
        let r = with_call("1", reg.call(&e.ctx, &c)).await.0;
        assert!(r.is_error, "{}", r.content);
        assert!(r.content.contains("changed since it was approved"), "{}", r.content);
        assert!(r.content.contains("nothing was run"));
        assert!(!e.ws.join("pwned").exists());
        assert_eq!(shell.plans_held(), 0);
        // Asked again: the new plan has a different key.
        let k2 = key(&e, &reg, &c).unwrap();
        assert_ne!(k.key, k2.key);
        // A deleted script is refused too.
        std::fs::remove_file(e.ws.join("a.py")).unwrap();
        let r = with_call("1", reg.call(&e.ctx, &c)).await.0;
        assert!(r.is_error && r.content.contains("nothing was run"), "{}", r.content);
        assert!(!e.ws.join("pwned").exists());
    }

    #[tokio::test]
    async fn an_unchanged_script_runs_exactly_the_planned_file_with_its_arguments() {
        if !have_python() {
            return;
        }
        let e = env();
        std::fs::write(
            e.ws.join("a.py"),
            "import sys; print('args', sys.argv[1:]); print(__file__)",
        )
        .unwrap();
        let (reg, shell) = registry(Claim::new(verified(), roots(&e)));
        let c = call("1", "python3 a.py -n 5 x");
        key(&e, &reg, &c).unwrap();
        assert_eq!(shell.plans_held(), 1);
        let r = with_call("1", reg.call(&e.ctx, &c)).await.0;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("args ['-n', '5', 'x']"), "{}", r.content);
        let file = e.ws.join("a.py").display().to_string();
        assert!(r.content.contains(&file), "{}", r.content);
        assert_eq!(shell.plans_held(), 0, "a call uses up its plan");
    }

    #[tokio::test]
    async fn a_script_runs_isolated_so_a_planted_module_cannot_shadow_the_stdlib() {
        if !have_python() {
            return;
        }
        let e = env();
        // Planted next to an approved, stdlib-only script.
        std::fs::write(e.ws.join("csv.py"), "print('PLANTED csv')").unwrap();
        std::fs::write(e.ws.join("json.py"), "print('PLANTED json')").unwrap();
        std::fs::write(
            e.ws.join("a.py"),
            "import csv, json, sys\nprint('csv from', csv.__file__)\nprint('isolated', sys.flags.isolated)\nprint('argv', sys.argv[1:])",
        )
        .unwrap();
        let (reg, _) = registry(Claim::new(verified(), roots(&e)));
        let c = call("1", "python3 a.py -n 2 y");
        key(&e, &reg, &c).unwrap();
        let r = with_call("1", reg.call(&e.ctx, &c)).await.0;
        assert!(!r.is_error, "{}", r.content);
        assert!(!r.content.contains("PLANTED"), "{}", r.content);
        assert!(!r.content.contains(&e.ws.display().to_string()), "csv came from the workspace: {}", r.content);
        assert!(r.content.contains("isolated 1"), "{}", r.content);
        assert!(r.content.contains("argv ['-n', '2', 'y']"), "{}", r.content);
        // Helper modules next to the script are not importable under a grant.
        std::fs::write(e.ws.join("helper.py"), "X = 1").unwrap();
        std::fs::write(e.ws.join("b.py"), "import helper\nprint(helper.X)").unwrap();
        let c = call("2", "python3 b.py");
        key(&e, &reg, &c).unwrap();
        let r = with_call("2", reg.call(&e.ctx, &c)).await.0;
        assert!(r.is_error && r.content.contains("ModuleNotFoundError"), "{}", r.content);
    }

    #[tokio::test]
    async fn concurrent_identical_calls_each_use_their_own_plan() {
        if !have_python() {
            return;
        }
        let e = env();
        std::fs::write(e.ws.join("a.py"), "print('ok')").unwrap();
        let (reg, shell) = registry(Claim::new(verified(), roots(&e)));
        let calls: Vec<ToolCall> = (0..4)
            .map(|i| call(&format!("c{i}"), "python3 a.py"))
            .collect();
        for c in &calls {
            key(&e, &reg, c).unwrap();
        }
        assert_eq!(shell.plans_held(), 4);
        let runs = calls.iter().map(|c| {
            let reg = &reg;
            let ctx = &e.ctx;
            async move { with_call(&c.id, reg.call(ctx, c)).await.0 }
        });
        let results = futures::future::join_all(runs).await;
        assert!(
            results
                .iter()
                .all(|r| !r.is_error && r.content.contains("ok")),
            "{results:?}"
        );
        assert_eq!(shell.plans_held(), 0);
        // A spent call id has no plan, so no key.
        assert_eq!(reg.grant_key("run", &calls[0]), None);
    }

    #[test]
    fn plans_of_calls_that_do_not_run_and_of_ended_runs_are_dropped() {
        let e = env();
        std::fs::write(e.ws.join("a.py"), "x").unwrap();
        let (reg, shell) = registry(Claim::new(verified(), roots(&e)));
        for id in ["1", "2", "3"] {
            key(&e, &reg, &call(id, "python3 a.py")).unwrap();
        }
        assert_eq!(shell.plans_held(), 3);
        reg.forget_plans("run", Some("2"));
        assert_eq!(shell.plans_held(), 2);
        reg.forget_plans("other-run", None);
        assert_eq!(shell.plans_held(), 2);
        reg.forget_plans("run", None);
        assert_eq!(shell.plans_held(), 0);
    }

    #[tokio::test]
    async fn a_call_decided_by_a_grant_runs_its_plan_or_nothing() {
        if !have_python() {
            return;
        }
        let e = env();
        let out = tempfile::tempdir().unwrap();
        let marker = out.path().join("ran");
        std::fs::write(
            out.path().join("o.py"),
            format!("open({:?}, 'w').write('x')", marker.display().to_string()),
        )
        .unwrap();
        std::fs::write(e.ws.join("a.py"), "print('fine')").unwrap();
        let (reg, shell) = registry(Claim::new(verified(), roots(&e)));

        // (b) approved by a grant, then the script became a link out of the
        // workspace: the typed command must not run through `sh`.
        let c = call("1", "python3 a.py");
        key(&e, &reg, &c).unwrap();
        reg.grant_applied("run", &c);
        std::fs::remove_file(e.ws.join("a.py")).unwrap();
        std::os::unix::fs::symlink(out.path().join("o.py"), e.ws.join("a.py")).unwrap();
        // The call is planned again (a probe, a replay): the plan no longer
        // resolves, but the grant decision stays with the call.
        reg.policy(&e.ctx, &c);
        assert_eq!(reg.grant_key("run", &c), None);
        let r = with_call("1", reg.call(&e.ctx, &c)).await.0;
        assert!(r.is_error && r.content.contains("covered by a grant"), "{}", r.content);
        assert!(r.content.contains("nothing was run"));
        assert!(!marker.exists(), "the typed command ran");
        assert_eq!(shell.plans_held(), 0);

        // A re-plan over changed bytes (a different key) isn't what the grant covered.
        std::fs::remove_file(e.ws.join("a.py")).unwrap();
        std::fs::write(e.ws.join("a.py"), "print('v1')").unwrap();
        let c = call("2", "python3 a.py");
        key(&e, &reg, &c).unwrap();
        reg.grant_applied("run", &c);
        std::fs::write(e.ws.join("a.py"), "open('pwned','w').write('x')").unwrap();
        reg.policy(&e.ctx, &c);
        let r = with_call("2", reg.call(&e.ctx, &c)).await.0;
        assert!(r.is_error && r.content.contains("covered by a grant"), "{}", r.content);
        assert!(!e.ws.join("pwned").exists());

        // A grant decided call with no plan at all is refused too.
        let c = call("3", "python3 missing.py");
        reg.policy(&e.ctx, &c);
        reg.grant_applied("run", &c);
        let r = with_call("3", reg.call(&e.ctx, &c)).await.0;
        assert!(r.is_error && r.content.contains("nothing was run"), "{}", r.content);

        // A human's answer is different: the typed command runs, as today
        // (here python reports the missing file, which is what was approved).
        let c = call("4", "python3 missing.py");
        reg.policy(&e.ctx, &c);
        let r = with_call("4", reg.call(&e.ctx, &c)).await.0;
        assert!(r.content.contains("missing.py"), "{}", r.content);
        assert!(!r.content.contains("covered by a grant"), "{}", r.content);
        let c = call("5", "echo typed");
        reg.policy(&e.ctx, &c);
        let r = with_call("5", reg.call(&e.ctx, &c)).await.0;
        assert!(r.content.contains("typed") && !r.is_error, "{}", r.content);
    }

    #[tokio::test]
    async fn ordinary_commands_are_untouched() {
        let e = env();
        let (reg, shell) = registry(Claim::new(verified(), roots(&e)));
        let c = call("1", "echo hello | tr a-z A-Z");
        let p = reg.policy(&e.ctx, &c);
        assert!(p.requires_approval && p.effect == Effect::Destructive);
        assert_eq!(shell.plans_held(), 0);
        let r = with_call("1", reg.call(&e.ctx, &c)).await.0;
        assert!(r.content.contains("HELLO"), "{}", r.content);
    }
}
