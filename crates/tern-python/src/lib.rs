//! The `python` tool (docs/python-tool-spec.md): compute in Monty, a Python
//! subset written in Rust, whose scripts call tern's read-only tools as
//! functions. The script itself has no network, environment, processes or
//! writable files; everything it learns comes through a tool call the agent
//! loop checks and journals ([`tern_core::script`]).
//!
//! The daemon runs each script in a `tern __monty-exec` child process
//! ([`helper`], PY1), so a crash in the interpreter ends that process, not
//! tern. In-process mode remains for tests and embedders without a helper.

mod convert;
mod errors;
pub mod helper;
mod modules;
mod scan;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use monty::{FunctionCall, MontyRun, RunProgress};
use monty_fs::{MountCallOutcome, MountMode, MountTable};
use monty_types::{
    CompileOptions, DateTimeSource, ExcType, ExtFunctionResult, MontyException, MontyObject,
    NameLookupResult, OsFunctionCall, OsPolicy, PrintWriter, RandomSeed, RandomStart,
    ResourceLimits, ResourceTracker, SleepMode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tern_core::script::{self, HostReply, ScriptEnv, ScriptHost};
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult, truncate_middle};
use tern_core::types::ToolSpec;
use tokio::sync::{mpsc, oneshot};

pub use helper::{HELPER_ARG, Helper};

/// Where the workspace appears inside scripts, read-only.
pub const WORKSPACE_MOUNT: &str = "/workspace";

/// What one script may use (docs/python-tool-spec.md §5.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Limits {
    /// Interpreter time for the whole script, excluding time suspended on
    /// tool calls.
    pub cpu: Duration,
    /// Interpreter time between two tool calls.
    pub turn: Duration,
    /// Wall time for the whole call, tool calls included.
    pub wall: Duration,
    /// Bytes Monty may allocate for the script's values.
    pub memory: usize,
    pub host_calls: usize,
    pub code_bytes: usize,
    /// Of the result, how much `print` output may take.
    pub printed_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            cpu: Duration::from_secs(30),
            turn: Duration::from_secs(10),
            wall: Duration::from_secs(600),
            memory: 256 * 1024 * 1024,
            host_calls: 200,
            code_bytes: 64 * 1024,
            printed_bytes: 16 * 1024,
        }
    }
}

pub struct Python {
    limits: Limits,
    /// Where scripts run: `None` is in this process.
    helper: Option<helper::Helper>,
}

impl Python {
    /// Scripts run in this process (tests, embedders). The daemon uses
    /// [`Python::with_helper`].
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            helper: None,
        }
    }

    /// Scripts run in a child process started by `helper`.
    pub fn with_helper(limits: Limits, helper: helper::Helper) -> Self {
        Self {
            limits,
            helper: Some(helper),
        }
    }
}

impl Default for Python {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

#[derive(Deserialize)]
struct Args {
    code: String,
    #[serde(default)]
    inputs: serde_json::Map<String, Value>,
}

pub const DESCRIPTION: &str = "\
Run a Python script for computation: loops, filtering, grouping, sums and statistics, parsing JSON or CSV, \
regex, dates. Prefer this to shell + python3 for compute. The script can call your read-only tools as \
functions by their tool name (e.g. `lua__county__lookup(address=a)` in a loop); each call is checked and \
recorded like a direct call. Files under /workspace are readable. There is no network, no shell and no \
packages. Return a summary as the last expression (a dict or list), not raw data; print() output comes back \
too. Tool calls return values, unless the script uses async/await/asyncio: then each call returns an \
awaitable, and `await asyncio.gather(*[f(x) for x in xs])` runs many at once (a few at a time); \
gather has no return_exceptions, so catch errors inside a small async function. A call that needs the \
user's approval waits for it, and the user sees it came from your script; a change nobody approves in \
time is queued (the call returns {\"queued\": true, \"outbox\": id}). Python subset: no class \
inheritance, generators, `del`, @property/@classmethod, or modules like enum, uuid, operator; \
available: json, re, csv, math, statistics, string, datetime, collections, itertools, functools, \
decimal, random, hashlib, base64, bisect, tomllib, pathlib. For pandas or numpy use shell with python3 \
(it asks the user).";

#[async_trait]
impl Tool for Python {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: script::PYTHON.into(),
            description: DESCRIPTION.into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "The script. Its last expression is the result." },
                    "inputs": {
                        "type": "object",
                        "description": "Optional values, each available to the script as a global variable of that name."
                    }
                },
                "required": ["code"]
            }),
        }
    }

    fn effect(&self) -> Effect {
        Effect::Read
    }

    fn policy(&self, _ctx: &ToolCtx, _args: &Value) -> ToolPolicy {
        // Everything outside the interpreter is a tool call with its own
        // check, so running a script never asks and may be repeated.
        ToolPolicy::new(Effect::Read, true)
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let code = args["code"].as_str()?;
        let first = code.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        Some(format!("python: {}", first.trim()))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let args: Args = match serde_json::from_value(raw) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(format!("invalid arguments: {e}")),
        };
        if args.code.len() > self.limits.code_bytes {
            return ToolResult::error(format!(
                "the script is {} bytes; the limit is {}",
                args.code.len(),
                self.limits.code_bytes
            ));
        }
        let host = script::current_host();
        let env = match &host {
            Some(h) => match h.env().await {
                Some(env) => env,
                None => return ToolResult::error("the run stopped"),
            },
            // Outside the agent loop (tests, direct calls): no tool calls,
            // and nothing to replay.
            None => ScriptEnv {
                clock_secs: now_secs(),
                seed: 0,
            },
        };
        let job = Job {
            code: args.code,
            inputs: args.inputs,
            env,
            workspace: ctx.workspace.clone(),
            blocked: host
                .as_ref()
                .map(|h| h.blocked().clone())
                .unwrap_or_default(),
            callable: host
                .as_ref()
                .map(|h| {
                    h.callable()
                        .iter()
                        .map(|t| (t.name.clone(), t.clone()))
                        .collect()
                })
                .unwrap_or_default(),
            limits: self.limits.clone(),
        };
        // The wall limit counts tool calls but not time spent waiting for
        // the user to answer an approval (spec §3.4, §5.1).
        let started = std::time::Instant::now();
        let waited = host.clone();
        let running = run(job, host, self.helper.as_ref());
        tokio::pin!(running);
        let outcome = loop {
            let spent = started
                .elapsed()
                .saturating_sub(waited.as_ref().map(|h| h.waited()).unwrap_or_default());
            let left = self.limits.wall.saturating_sub(spent);
            if left.is_zero() {
                return ToolResult::error(format!(
                    "stopped: the script ran for more than {}s, tool calls included",
                    self.limits.wall.as_secs()
                ));
            }
            tokio::select! {
                o = &mut running => break o,
                _ = ctx.cancel.cancelled() => return ToolResult::error("cancelled"),
                _ = tokio::time::sleep(left) => {}
            }
        };
        outcome.render(self.limits.printed_bytes)
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Everything the interpreter needs: the first message to a helper.
#[derive(Serialize, Deserialize)]
pub(crate) struct Job {
    code: String,
    inputs: serde_json::Map<String, Value>,
    env: ScriptEnv,
    workspace: PathBuf,
    callable: BTreeMap<String, ToolSpec>,
    /// Tools the run has that scripts can't call, and why.
    blocked: BTreeMap<String, String>,
    limits: Limits,
}

/// Tool calls from the interpreter thread: one, or several that a script
/// awaits together.
struct Request {
    calls: Vec<(String, Value)>,
    reply: oneshot::Sender<Vec<Reply>>,
}

/// The host's answer to one tool call, as the interpreter sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum Reply {
    Done {
        content: String,
        is_error: bool,
    },
    Refused(String),
    /// The script ends with this error (a re-run that diverged after
    /// writes, spec §4.2).
    Abort(String),
}

impl From<&HostReply> for Reply {
    fn from(r: &HostReply) -> Self {
        match r {
            HostReply::Done(r) => Reply::Done {
                content: r.content.clone(),
                is_error: r.is_error,
            },
            HostReply::Refused(why) => Reply::Refused(why.clone()),
            HostReply::Abort(why) => Reply::Abort(why.clone()),
        }
    }
}

/// Ask the agent loop (through `host`) to run these calls together.
pub(crate) async fn ask_host(
    host: Option<&ScriptHost>,
    calls: Vec<(String, Value)>,
) -> Vec<HostReply> {
    match host {
        Some(h) => h.calls(calls).await,
        None => vec![HostReply::Refused("tools can't be called here".into()); calls.len()],
    }
}

/// Replies for the interpreter, noting whether any result was untrusted.
pub(crate) fn replies(answers: &[HostReply], untrusted: &mut bool) -> Vec<Reply> {
    for a in answers {
        if let HostReply::Done(r) = a {
            *untrusted |= r.untrusted;
        }
    }
    answers.iter().map(Reply::from).collect()
}

/// How a script ended.
#[derive(Serialize, Deserialize)]
pub(crate) struct Outcome {
    /// The last expression, as JSON; `None` for no value.
    result: Result<Option<Value>, String>,
    printed: String,
    /// Per tool: calls that succeeded, and that failed or were refused.
    calls: BTreeMap<String, (usize, usize)>,
    untrusted: bool,
}

impl Outcome {
    fn render(self, printed_limit: usize) -> ToolResult {
        let mut out = String::new();
        let is_error = self.result.is_err();
        match self.result {
            Ok(Some(v)) => {
                out.push_str("result:\n");
                out.push_str(&serde_json::to_string(&v).unwrap_or_else(|_| v.to_string()));
                out.push('\n');
            }
            Ok(None) => {}
            Err(e) => {
                out.push_str("error:\n");
                out.push_str(e.trim_end());
                out.push('\n');
            }
        }
        if !self.printed.is_empty() {
            out.push_str("printed:\n");
            out.push_str(truncate_middle(&self.printed, printed_limit).trim_end());
            out.push('\n');
        }
        if !self.calls.is_empty() {
            let calls: Vec<String> = self
                .calls
                .iter()
                .map(|(name, (ok, failed))| {
                    let n = ok + failed;
                    if *failed == 0 {
                        format!("{name} ×{n}")
                    } else {
                        format!("{name} ×{n} ({failed} failed)")
                    }
                })
                .collect();
            out.push_str("tool calls: ");
            out.push_str(&calls.join(", "));
            out.push('\n');
        }
        if out.is_empty() {
            out.push_str("(no result and nothing printed)");
        }
        ToolResult {
            content: out,
            is_error,
            untrusted: self.untrusted,
        }
    }
}

impl Outcome {
    pub(crate) fn failed(why: impl Into<String>) -> Self {
        Self {
            result: Err(why.into()),
            printed: String::new(),
            calls: BTreeMap::new(),
            untrusted: false,
        }
    }
}

/// Run `job` in a helper process when there is one, else on a thread here.
async fn run(job: Job, host: Option<Arc<ScriptHost>>, helper: Option<&helper::Helper>) -> Outcome {
    match helper {
        Some(h) => h.run(job, host.as_deref()).await,
        None => run_in_thread(job, host).await,
    }
}

/// Run `job` on its own thread, answering its tool calls through `host`.
async fn run_in_thread(job: Job, host: Option<Arc<ScriptHost>>) -> Outcome {
    let (tx, mut rx) = mpsc::channel::<Request>(1);
    let (done_tx, mut done) = oneshot::channel();
    // Monty recurses on the native stack (its frame limit is 1000, and
    // callbacks like sorted(key=…) recurse further): give it room.
    let spawned = std::thread::Builder::new()
        .name("tern-python".into())
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let mut bridge = |calls: Vec<(String, Value)>| {
                let n = calls.len();
                let stopped = || vec![Reply::Refused("the run stopped".into()); n];
                let (reply, rx) = oneshot::channel();
                match tx.blocking_send(Request { calls, reply }) {
                    Ok(()) => rx.blocking_recv().unwrap_or_else(|_| stopped()),
                    Err(_) => stopped(),
                }
            };
            let _ = done_tx.send(interpret(&job, &mut bridge));
        });
    if let Err(e) = spawned {
        return Outcome::failed(format!("could not start the interpreter: {e}"));
    }
    let mut untrusted = false;
    loop {
        tokio::select! {
            o = &mut done => {
                let mut o = o
                    .unwrap_or_else(|_| Outcome::failed("the interpreter stopped unexpectedly"));
                o.untrusted |= untrusted;
                return o;
            }
            Some(req) = rx.recv() => {
                let answers = ask_host(host.as_deref(), req.calls).await;
                let _ = req.reply.send(replies(&answers, &mut untrusted));
            }
        }
    }
}

/// Answers tool calls for the interpreter (one answer per call, in order): a
/// channel to the async side in this process, or the helper's pipe to the
/// daemon.
pub(crate) type Bridge<'a> = dyn FnMut(Vec<(String, Value)>) -> Vec<Reply> + 'a;

/// The interpreter loop, on the script's thread.
pub(crate) fn interpret(job: &Job, bridge: &mut Bridge<'_>) -> Outcome {
    let mut printed = String::new();
    let mut calls = BTreeMap::new();
    let result = drive(job, bridge, &mut printed, &mut calls);
    Outcome {
        result: result.map_err(|e| errors::explain(&e, &job.callable)),
        printed,
        calls,
        // Set by the async side, which sees the results.
        untrusted: false,
    }
}

/// What a call to a function the script can see turns out to be.
enum Verdict {
    /// Answered here: one of tern's own stdlib functions.
    Local(ExtFunctionResult),
    /// Fails without reaching the host: blocked, unknown, or badly called.
    Fail(ExtFunctionResult),
    /// Ends the script.
    Abort(MontyException),
    /// A tool call for the agent loop.
    Host(String, Value),
}

/// A call whose result a script awaits later.
enum Slot {
    Call(String, Value),
    /// Failed before reaching the host; raises when awaited.
    Ready(ExtFunctionResult),
}

fn classify(
    job: &Job,
    call: &FunctionCall,
    made: &mut usize,
    calls: &mut BTreeMap<String, (usize, usize)>,
) -> Verdict {
    let name = &call.function_name;
    if call.object_id.is_none() && modules::provides(name) {
        return Verdict::Local(modules::call(name, &call.args));
    }
    if let Some(why) = job.blocked.get(name).filter(|_| call.object_id.is_none()) {
        calls.entry(name.clone()).or_default().1 += 1;
        return Verdict::Fail(ExtFunctionResult::Error(MontyException::new(
            ExcType::PermissionError,
            Some(format!("{name}: {why}")),
        )));
    }
    if call.object_id.is_some() || !job.callable.contains_key(name) {
        return Verdict::Fail(ExtFunctionResult::Error(errors::unknown_function(
            name,
            &job.callable,
        )));
    }
    if *made >= job.limits.host_calls {
        // Monty leaves counting suspensions to embedders.
        return Verdict::Abort(MontyException::new(
            ExcType::RuntimeError,
            Some(format!(
                "{} {}",
                errors::HOST_CALL_LIMIT,
                job.limits.host_calls
            )),
        ));
    }
    *made += 1;
    match convert::call_input(&call.args, &job.callable[name]) {
        Err(msg) => Verdict::Fail(ExtFunctionResult::Error(MontyException::new(
            ExcType::TypeError,
            Some(msg),
        ))),
        Ok(input) => Verdict::Host(name.clone(), input),
    }
}

fn drive(
    job: &Job,
    bridge: &mut Bridge<'_>,
    printed: &mut String,
    calls: &mut BTreeMap<String, (usize, usize)>,
) -> Result<Option<Value>, MontyException> {
    let names: Vec<String> = job.inputs.keys().cloned().collect();
    let values: Vec<MontyObject> = job.inputs.values().map(convert::to_monty).collect();
    let runner = MontyRun::new(
        job.code.clone(),
        "main.py",
        names,
        CompileOptions::default(),
    )?
    .with_os_policy(OsPolicy {
        datetime: DateTimeSource::Fixed {
            unix_seconds: job.env.clock_secs,
            microsecond: 0,
        },
        sleep: SleepMode::Zero,
        random_start: RandomStart::Seed(RandomSeed::Int(job.env.seed.into())),
        ..OsPolicy::default()
    });
    let limits = ResourceLimits {
        max_feed_duration: Some(job.limits.cpu),
        max_turn_duration: Some(job.limits.turn),
        max_memory: Some(job.limits.memory),
        ..ResourceLimits::default()
    };
    let mut mounts = MountTable::new();
    if job.workspace.is_dir()
        && let Err(e) = mounts.mount(WORKSPACE_MOUNT, &job.workspace, MountMode::ReadOnly, None)
    {
        tracing::warn!(error = %e, "python: workspace not mounted");
    }

    // A script that uses `async` gets awaitables from tool calls (spec
    // §3.2): they are collected and run together when it awaits them, which
    // is what `asyncio.gather` is for. Other scripts get values.
    let async_mode = scan::uses_async(&job.code);
    let mut pending: BTreeMap<u32, Slot> = BTreeMap::new();
    let mut made = 0usize;
    let mut progress = runner.start(
        values,
        ResourceTracker::new(limits),
        PrintWriter::collect_string(printed),
    )?;
    loop {
        progress = match progress {
            RunProgress::Complete(v) => {
                return Ok((v.type_name() != "NoneType").then(|| convert::to_json(&v)));
            }
            RunProgress::FunctionCall(call) => match classify(job, &call, &mut made, calls) {
                Verdict::Local(r) => call.resume(r, PrintWriter::collect_string(printed))?,
                Verdict::Abort(exc) => call.abort(exc, PrintWriter::collect_string(printed))?,
                Verdict::Fail(r) if async_mode => {
                    pending.insert(call.call_id, Slot::Ready(r));
                    call.resume_pending(PrintWriter::collect_string(printed))?
                }
                Verdict::Fail(r) => call.resume(r, PrintWriter::collect_string(printed))?,
                Verdict::Host(name, input) if async_mode && !call.allow_eager_await => {
                    // Not awaited yet: it runs with whatever else the script
                    // collects, when something awaits it.
                    pending.insert(call.call_id, Slot::Call(name, input));
                    call.resume_pending(PrintWriter::collect_string(printed))?
                }
                Verdict::Host(name, input) => {
                    // A plain call, or an `await` on the spot: run it now.
                    let answer = bridge(vec![(name.clone(), input)])
                        .pop()
                        .unwrap_or_else(|| Reply::Refused("the run stopped".into()));
                    match host_result(&name, answer, calls) {
                        Err(exc) => call.abort(exc, PrintWriter::collect_string(printed))?,
                        Ok(result) if async_mode => call.resume_eager(
                            match result {
                                ExtFunctionResult::Return(v) => Ok(v),
                                ExtFunctionResult::Error(e) => Err(e),
                                _ => Err(MontyException::new(
                                    ExcType::RuntimeError,
                                    Some("unexpected tool result".into()),
                                )),
                            },
                            PrintWriter::collect_string(printed),
                        )?,
                        Ok(result) => call.resume(result, PrintWriter::collect_string(printed))?,
                    }
                }
            },
            RunProgress::OsCall(os) => os
                .resume_with(PrintWriter::collect_string(printed), |c| {
                    os_call(&mut mounts, c)
                })?,
            RunProgress::NameLookup(lookup) => lookup.resume(
                NameLookupResult::Undefined,
                PrintWriter::collect_string(printed),
            )?,
            RunProgress::ResolveFutures(wait) => {
                // Everything the script is waiting on runs together, and
                // all the results go back at once, in call order, so what
                // the script sees doesn't depend on which tool was faster
                // (a re-run after a crash sees the same).
                let mut ids = wait.pending_call_ids().to_vec();
                ids.sort_unstable();
                let mut results: Vec<(u32, ExtFunctionResult)> = Vec::new();
                let mut batch_ids = Vec::new();
                let mut batch = Vec::new();
                for id in ids {
                    match pending.remove(&id) {
                        Some(Slot::Ready(r)) => results.push((id, r)),
                        Some(Slot::Call(name, input)) => {
                            batch_ids.push(id);
                            batch.push((name, input));
                        }
                        None => results.push((
                            id,
                            ExtFunctionResult::Error(MontyException::new(
                                ExcType::RuntimeError,
                                Some(errors::NOT_A_TOOL_CALL.into()),
                            )),
                        )),
                    }
                }
                let names: Vec<String> = batch.iter().map(|(n, _)| n.clone()).collect();
                let mut aborted = None;
                if !batch.is_empty() {
                    let mut answers = bridge(batch);
                    answers.resize_with(names.len(), || Reply::Refused("the run stopped".into()));
                    for ((id, name), answer) in batch_ids.into_iter().zip(names).zip(answers) {
                        match host_result(&name, answer, calls) {
                            Ok(r) => results.push((id, r)),
                            Err(exc) => {
                                aborted.get_or_insert(exc);
                            }
                        }
                    }
                }
                if let Some(exc) = aborted {
                    wait.abort(exc, PrintWriter::collect_string(printed))?
                } else {
                    results.sort_by_key(|(id, _)| *id);
                    wait.resume(results, PrintWriter::collect_string(printed))?
                }
            }
        };
    }
}

/// Turn the loop's answer to one tool call into Python's: the value, or an
/// exception the script can catch. `Err` ends the script.
fn host_result(
    name: &str,
    answer: Reply,
    calls: &mut BTreeMap<String, (usize, usize)>,
) -> Result<ExtFunctionResult, MontyException> {
    let tally = calls.entry(name.to_owned()).or_default();
    Ok(match answer {
        Reply::Done { content, is_error } if !is_error => {
            tally.0 += 1;
            ExtFunctionResult::Return(convert::tool_output(&content))
        }
        Reply::Done { content, .. } => {
            tally.1 += 1;
            let first = content.lines().next().unwrap_or("").trim();
            ExtFunctionResult::Error(MontyException::new(
                ExcType::RuntimeError,
                Some(format!("{name}: {first}")),
            ))
        }
        Reply::Refused(why) => {
            tally.1 += 1;
            ExtFunctionResult::Error(MontyException::new(
                ExcType::PermissionError,
                Some(format!("{name}: {why}")),
            ))
        }
        Reply::Abort(why) => {
            tally.1 += 1;
            return Err(MontyException::new(ExcType::RuntimeError, Some(why)));
        }
    })
}

/// Filesystem calls go to the read-only workspace mount; nothing else
/// (environment, other paths) exists.
fn os_call(mounts: &mut MountTable, call: OsFunctionCall) -> ExtFunctionResult {
    match mounts.handle_os_call(call) {
        MountCallOutcome::Handled(Ok(v)) => ExtFunctionResult::Return(v),
        MountCallOutcome::Handled(Err(e)) => ExtFunctionResult::Error(e.into_exception()),
        MountCallOutcome::NotHandled(OsFunctionCall::Import(module)) => {
            match modules::import(&module) {
                Some(m) => ExtFunctionResult::Return(m),
                None => ExtFunctionResult::NotFound(module),
            }
        }
        MountCallOutcome::NotHandled(other) => ExtFunctionResult::Error(MontyException::new(
            ExcType::PermissionError,
            Some(format!(
                "{} isn't available in tern's python: only files under {WORKSPACE_MOUNT} can be read",
                errors::os_call_name(&other)
            )),
        )),
    }
}

#[cfg(test)]
mod tests;
