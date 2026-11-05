//! Running scripts out of process (docs/python-tool-spec.md §5.2, PY1).
//!
//! The daemon starts `<tern> __monty-exec` for each script, under the shell's
//! OS sandbox with the network off, and talks to it in JSON lines:
//!
//! ```text
//! daemon → helper  a Job (code, inputs, clock and seed, tools, limits)
//! helper → daemon  {"Calls": [{"name": …, "input": …}, …]}  tool calls
//! daemon → helper  [{"Done": {…}} | {"Refused": "…"} | {"Abort": "…"}, …]
//!                  one answer per call, in order
//! helper → daemon  {"Finished": Outcome}                 the end
//! ```
//!
//! Journaling, approvals and taint stay in the daemon; the helper only
//! interprets. A crash, a native stack overflow or the memory ceiling ends
//! the helper, and the tool returns an error.

use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tern_core::script::ScriptHost;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::{Job, Outcome, Reply, ask_host, interpret, replies};

/// The argument `main` checks before parsing the command line.
pub const HELPER_ARG: &str = "__monty-exec";

/// Set in the daemon's environment by tests: the helper aborts before
/// interpreting, as a native crash would.
pub const CRASH_ENV: &str = "TERN_TEST_PYTHON_CRASH";

/// Exit code of a helper whose memory ceiling refused an allocation
/// (`monty-alloc` with its `exit-code` feature).
const OOM_EXIT: i32 = monty_types::OOM_EXIT_CODE;

#[derive(Serialize, Deserialize)]
enum FromHelper {
    /// One call, or several the script awaits together (`gather`).
    Calls(Vec<CallRequest>),
    Finished(Outcome),
}

#[derive(Serialize, Deserialize)]
struct CallRequest {
    name: String,
    input: Value,
}

/// How the daemon starts helpers.
#[derive(Clone)]
pub struct Helper {
    /// The tern binary (`current_exe()` in the daemon).
    pub program: PathBuf,
    /// Confines the helper; `None` runs it unconfined.
    pub sandbox: Option<Arc<tern_sandbox::HostBackend>>,
}

impl Helper {
    pub(crate) async fn run(&self, job: Job, host: Option<&ScriptHost>) -> Outcome {
        match self.exchange(job, host).await {
            Ok(o) => o,
            Err(e) => Outcome::failed(e),
        }
    }

    async fn exchange(&self, job: Job, host: Option<&ScriptHost>) -> Result<Outcome, String> {
        let program: Vec<OsString> = vec![self.program.clone().into(), HELPER_ARG.into()];
        let argv = match &self.sandbox {
            Some(sb) => sb
                .confine_argv_reading(
                    program,
                    &job.workspace,
                    &job.read_roots
                        .iter()
                        .map(|(_, p)| p.clone())
                        .collect::<Vec<_>>(),
                )
                .map_err(|e| format!("could not start the interpreter: {e}"))?,
            None => program,
        };
        let (bin, args) = argv
            .split_first()
            .ok_or("could not start the interpreter: empty command")?;
        let mut cmd = tokio::process::Command::new(bin);
        cmd.args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if job.workspace.is_dir() {
            cmd.current_dir(&job.workspace);
        }
        if let Some(v) = std::env::var_os(CRASH_ENV) {
            cmd.env(CRASH_ENV, v);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("could not start the interpreter: {e}"))?;
        let (Some(mut stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err("could not start the interpreter: no pipes".into());
        };
        // Drain stderr so a chatty helper can't block on it.
        let errors = tokio::spawn(async move {
            let mut buf = Vec::new();
            let _ = (&mut stderr).take(64 * 1024).read_to_end(&mut buf).await;
            String::from_utf8_lossy(&buf).into_owned()
        });

        send(&mut stdin, &job).await?;
        let mut lines = BufReader::new(stdout).lines();
        let mut untrusted = false;
        let finished = loop {
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) | Err(_) => break None,
            };
            match serde_json::from_str::<FromHelper>(&line) {
                Ok(FromHelper::Calls(calls)) => {
                    let calls = calls.into_iter().map(|c| (c.name, c.input)).collect();
                    let answers = ask_host(host, calls).await;
                    // A helper that died mid-call shows up as EOF next.
                    let _ = send(&mut stdin, &replies(&answers, &mut untrusted)).await;
                }
                Ok(FromHelper::Finished(o)) => break Some(o),
                Err(e) => return Err(format!("the interpreter sent something unreadable: {e}")),
            }
        };
        drop(stdin);
        let status = child.wait().await.ok();
        let stderr = errors.await.unwrap_or_default();
        match finished {
            Some(mut o) => {
                o.untrusted |= untrusted;
                Ok(o)
            }
            None => {
                let mut o = Outcome::failed(died(status, &stderr));
                o.untrusted = untrusted;
                Ok(o)
            }
        }
    }
}

async fn send<T: Serialize>(stdin: &mut tokio::process::ChildStdin, msg: &T) -> Result<(), String> {
    let mut line = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
    line.push(b'\n');
    stdin
        .write_all(&line)
        .await
        .map_err(|e| format!("the interpreter stopped: {e}"))?;
    stdin.flush().await.map_err(|e| e.to_string())
}

/// What the model is told when the helper ended without an answer.
fn died(status: Option<std::process::ExitStatus>, stderr: &str) -> String {
    let last = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let detail = if last.is_empty() {
        String::new()
    } else {
        format!(" ({})", last.trim())
    };
    if status.and_then(|s| s.code()) == Some(OOM_EXIT) {
        return format!(
            "MemoryError: the script went over its memory limit and was stopped{detail}\n\
             hint: the script hit its memory limit: keep less data in memory"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        match status.and_then(|s| s.signal()) {
            Some(nix::libc::SIGXCPU) => {
                return "TimeoutError: the script used its CPU limit and was stopped\n\
                        hint: the script ran out of interpreter time: do less work per script or \
                        use a cheaper algorithm"
                    .into();
            }
            Some(sig) => {
                return format!(
                    "the python interpreter crashed (signal {sig}){detail}. tern itself is fine. \
                     Try a simpler script, or do this step with your other tools."
                );
            }
            None => {}
        }
    }
    let code = status
        .and_then(|s| s.code())
        .map_or("?".to_owned(), |c| c.to_string());
    format!(
        "the python interpreter stopped unexpectedly (exit {code}){detail}. tern itself is fine. \
         Try a simpler script, or do this step with your other tools."
    )
}

/// `tern __monty-exec`: read one job from stdin, interpret it, answer tool
/// calls over stdin/stdout, and exit. Returns `None` when the process wasn't
/// started as a helper.
pub fn run_if_requested() -> Option<ExitCode> {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new(HELPER_ARG)) {
        return None;
    }
    Some(helper_main())
}

fn helper_main() -> ExitCode {
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return ExitCode::FAILURE;
    }
    let job: Job = match serde_json::from_str(&line) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("tern __monty-exec: bad job: {e}");
            return ExitCode::FAILURE;
        }
    };
    if std::env::var_os(CRASH_ENV).is_some() {
        std::process::abort();
    }
    limit_self(&job);

    let stdout = std::io::stdout();
    // Monty recurses on the native stack: give it room, as in-process.
    let worker = std::thread::Builder::new()
        .name("tern-python".into())
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            // Stdin buffers internally, so what follows the job line is kept.
            let mut reader = std::io::stdin().lock();
            let mut out = stdout.lock();
            let mut bridge = |calls: Vec<(String, Value)>| -> Vec<Reply> {
                let n = calls.len();
                let refused = |why: String| vec![Reply::Refused(why); n];
                let msg = FromHelper::Calls(
                    calls
                        .into_iter()
                        .map(|(name, input)| CallRequest { name, input })
                        .collect(),
                );
                if write_line(&mut out, &msg).is_err() {
                    return refused("the run stopped".into());
                }
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(len) if len > 0 => match serde_json::from_str::<Vec<Reply>>(&line) {
                        Ok(r) if r.len() == n => r,
                        Ok(_) => refused("bad answer: wrong number of replies".into()),
                        Err(e) => refused(format!("bad answer: {e}")),
                    },
                    _ => refused("the run stopped".into()),
                }
            };
            let outcome = interpret(&job, &mut bridge);
            write_line(&mut out, &FromHelper::Finished(outcome)).is_ok()
        });
    match worker.map(|w| w.join()) {
        Ok(Ok(true)) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

fn write_line<T: Serialize>(out: &mut impl Write, msg: &T) -> std::io::Result<()> {
    serde_json::to_writer(&mut *out, msg)?;
    out.write_all(b"\n")?;
    out.flush()
}

/// Limits the OS enforces on the whole helper, behind Monty's own: the
/// memory ceiling (when `monty-alloc` is the binary's allocator) and CPU
/// time.
fn limit_self(job: &Job) {
    let budget = monty_types::memory_limit_with_headroom(Some(job.limits.memory), false);
    if let Err(e) = monty_alloc::set_hard_limit(budget) {
        // Built without the allocator (tests): Monty's own checks only.
        tracing::debug!("python helper: no memory ceiling: {e}");
    }
    #[cfg(unix)]
    {
        use nix::sys::resource::{Resource, setrlimit};
        // Interpreter time is Monty's limit; this catches what it can't see
        // (compiling, a runaway native loop). Tool calls wait, they don't
        // burn CPU.
        let secs = job.limits.cpu.as_secs() + 10;
        let _ = setrlimit(Resource::RLIMIT_CPU, secs, secs + 5);
    }
}
