//! Serving a script's tool calls (docs/python-tool-spec.md §3.3, §3.4, §3.5,
//! §4): each is a journaled `py_call` sub-step after the script's own `tool`
//! step at `idx`, checked like a top-level call. Sub-step `idx + 1` records
//! the script's clock and seed; the sub-steps that follow are, in the order
//! the script makes its calls, an *ask* (the approval, when the call needs
//! one) and then the call itself, so a script re-run after a crash is
//! answered from the journal.
//!
//! A call that needs the user's answer goes through the same `decide` as a
//! top-level call, so grants, taint, autonomy, the tainted host limit and the
//! outbox behave the same. The script waits suspended meanwhile.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tern_store::RunRow;

use super::taint::EgressState;
use super::{Agent, INTERRUPTED_TOOL_MESSAGE};
use crate::approval::{self, ApprovalOutcome, Autonomy, Check, Gate};
use crate::events::AgentEvent;
use crate::journal::{InFlight, Outcome, StepError, StepKind, StepOutput, input_hash};
use crate::script::{self, HostReply, HostRequest, ScriptEnv, ScriptHost, ScriptLimits};
use crate::tool::{Effect, ToolCtx, ToolResult};
use crate::types::ToolCall;
use crate::types::ToolSpec;

/// How much of the script an approval card carries.
const SCRIPT_CARD_BYTES: usize = 2000;

/// The loop's state while one script runs.
struct Serving<'a> {
    run: &'a RunRow,
    /// The script's own step.
    idx: u32,
    call_id: &'a str,
    scope: Option<&'a [String]>,
    ctx: &'a ToolCtx,
    callable: HashSet<String>,
    host: Arc<ScriptHost>,
    limits: ScriptLimits,
    /// The script's text, for approval cards.
    code: String,
    /// Sub-steps used so far.
    seq: u32,
    /// Tool calls the script has made (the "call 7" of an approval card).
    calls: u32,
    /// Approvals asked of the user so far, replayed ones included.
    asks: u32,
    /// Write calls let through so far.
    writes: u32,
    /// The run's taint, updated by the script's own untrusted results.
    tainted: bool,
    taint_reason: Option<String>,
    /// The run's autonomy (tooling spec §5.4), as for direct calls.
    autonomy: Autonomy,
    /// The tainted host limit, as for direct calls; hosts the script's own
    /// reads reach are added.
    egress: EgressState,
}

/// Starts the result of a script's network read that the tainted host
/// limit refused. Scripts used to be refused outright (PY0); those results
/// may still be in an old journal.
const EGRESS_REFUSED: &str = "refused: this run read untrusted content, and";

/// The output of an *ask* sub-step: the decision, and what the script is
/// told when the call doesn't run.
#[derive(Debug, Serialize, Deserialize)]
struct Asked {
    outcome: ApprovalOutcome,
    why: String,
}

/// What a journaled script sub-step says to the loop that comes after it.
#[derive(Debug, Default)]
pub(super) struct SubStep {
    /// The host a network read reached, if the call ran.
    pub host: Option<String>,
    /// The user chose "Allow all sites for this run".
    pub allow_all_egress: bool,
}

/// A call that passed its checks and runs as a sub-step.
struct Prepared {
    call: ToolCall,
    /// Journal index of its sub-step.
    step: u32,
    input: Value,
    in_flight: InFlight<ToolResult>,
    untrusted: bool,
    host: Option<String>,
    effect: Effect,
}

enum Planned {
    /// Answered without running: refused, queued, or aborting the script.
    Answer(HostReply),
    Run(Box<Prepared>),
}

/// A call that ran (or replayed), for folding into the script's state.
struct Executed {
    call: ToolCall,
    host: Option<String>,
    effect: Effect,
    result: Outcome<ToolResult>,
}

impl Agent {
    /// The tools a script in a run limited to `scope` may call, and why
    /// each other tool can't be.
    fn script_tools(&self, scope: Option<&[String]>) -> (Vec<ToolSpec>, BTreeMap<String, String>) {
        let mut callable = Vec::new();
        let mut blocked = BTreeMap::new();
        for spec in self.tools.specs() {
            match self.not_callable(scope, &spec.name) {
                None => callable.push(spec),
                Some(why) => {
                    blocked.insert(spec.name, why);
                }
            }
        }
        (callable, blocked)
    }

    /// Why a script can never call `name` in this run, or `None` if it may
    /// (the per-call check comes later).
    fn not_callable(&self, scope: Option<&[String]>, name: &str) -> Option<String> {
        let Some(tool) = self.tools.get(name) else {
            return Some("not a tool".into());
        };
        if !crate::tool::in_scope(scope, name) {
            return Some("not available to this agent".into());
        }
        let allow_writes = self.cfg.script.allow_writes;
        if script::callable_in_scripts(name, tool.effect(), allow_writes) {
            return None;
        }
        Some(match tool.effect() {
            Effect::Read => "this tool can't be used from a script; call it directly".into(),
            Effect::Write if script::NOT_IN_SCRIPTS.contains(&name) => {
                "this tool can't be used from a script; call it directly".into()
            }
            Effect::Write => "write tool; scripts can only call read-only tools unless \
                              `[python] allow_writes` is on; call it directly"
                .into(),
            Effect::Destructive => {
                "destructive tool; scripts can't call these. Call it directly".into()
            }
        })
    }

    /// Await `fut` (the tool call at step `idx`); for a script tool, serve
    /// its host calls meanwhile. A journal error in a sub-step fails the
    /// step once the script has stopped.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn serve_scripts<F, T>(
        &self,
        run: &RunRow,
        idx: u32,
        call: &ToolCall,
        scope: Option<&[String]>,
        ctx: &ToolCtx,
        gate: Gate,
        taint_reason: Option<&str>,
        egress: &EgressState,
        fut: F,
    ) -> Result<T, StepError>
    where
        F: Future<Output = T>,
    {
        if call.name != script::PYTHON {
            return Ok(fut.await);
        }
        let (callable, blocked) = self.script_tools(scope);
        let (host, mut rx) = ScriptHost::channel(callable.clone(), blocked);
        let mut s = Serving {
            run,
            idx,
            call_id: &call.id,
            scope,
            ctx,
            callable: callable.iter().map(|t| t.name.clone()).collect(),
            host: host.clone(),
            limits: self.cfg.script,
            code: call.input["code"].as_str().unwrap_or_default().to_owned(),
            seq: 0,
            calls: 0,
            asks: 0,
            writes: 0,
            tainted: gate.tainted,
            taint_reason: taint_reason.map(str::to_owned),
            autonomy: gate.autonomy,
            egress: egress.clone(),
        };
        let fut = script::with_host(host, fut);
        tokio::pin!(fut);
        let mut failure = None;
        loop {
            tokio::select! {
                out = &mut fut => return failure.map_or(Ok(out), Err),
                Some(req) = rx.recv() => {
                    if let Err(e) = self.serve(&mut s, req).await {
                        failure.get_or_insert(e);
                    }
                }
            }
        }
    }

    async fn serve(&self, s: &mut Serving<'_>, req: HostRequest) -> Result<(), StepError> {
        match req {
            HostRequest::Env(reply) => {
                s.seq += 1;
                let input = json!({ "in": s.idx, "seq": s.seq, "env": true });
                let env = self
                    .journal
                    .step(
                        &s.run.id,
                        s.idx + s.seq,
                        StepKind::PyCall,
                        &input,
                        || async {
                            Ok(ScriptEnv {
                                clock_secs: chrono::Utc::now().timestamp(),
                                seed: fastrand::u64(..),
                            })
                        },
                    )
                    .await?;
                let _ = reply.send(env);
            }
            HostRequest::Calls { calls, reply } => {
                let answers = self.serve_batch(s, calls).await?;
                let _ = reply.send(answers);
            }
        }
        Ok(())
    }

    /// Plan every call of a batch in the order the script made them (the
    /// checks, the asks and the sub-step numbers are sequential, so a
    /// replay sees the same ones), then run the approved ones up to
    /// `max_parallel` at a time. The checks all see the taint the script
    /// had when the batch began: calls made together can't depend on each
    /// other's results.
    async fn serve_batch(
        &self,
        s: &mut Serving<'_>,
        calls: Vec<(String, Value)>,
    ) -> Result<Vec<HostReply>, StepError> {
        let mut planned = Vec::with_capacity(calls.len());
        // Hosts an earlier call of this batch got approved to reach: as in
        // a sequence of calls, a later one to the same site doesn't ask.
        let mut reached = BTreeSet::new();
        for (name, input) in calls {
            s.calls += 1;
            let call = ToolCall {
                id: format!("{}.{}", s.call_id, s.calls),
                name,
                input,
            };
            planned.push(self.plan(s, call, &mut reached).await?);
        }
        if let Some(why) = planned.iter().find_map(|p| match p {
            Planned::Answer(HostReply::Abort(why)) => Some(why.clone()),
            _ => None,
        }) {
            // The script ends here: nothing in this batch runs.
            return Ok(planned
                .iter()
                .map(|_| HostReply::Abort(why.clone()))
                .collect());
        }

        let (run, scope, ctx) = (s.run, s.scope, s.ctx);
        // `Some` is an answer already; `None` waits for the next result.
        let mut slots: Vec<Option<HostReply>> = Vec::with_capacity(planned.len());
        let mut to_run = Vec::new();
        for p in planned {
            match p {
                Planned::Answer(a) => slots.push(Some(a)),
                Planned::Run(prep) => {
                    slots.push(None);
                    to_run.push(prep);
                }
            }
        }
        let parallel = s.limits.max_parallel.max(1);
        let executed: Vec<Result<Executed, StepError>> = futures::stream::iter(to_run)
            .map(|p| self.exec(run, scope, ctx, *p))
            .buffered(parallel)
            .collect()
            .await;

        let mut executed = executed.into_iter();
        let mut answers = Vec::with_capacity(slots.len());
        for slot in slots {
            match slot {
                Some(a) => answers.push(a),
                None => {
                    let Some(done) = executed.next() else {
                        answers.push(HostReply::Refused("the run stopped".into()));
                        continue;
                    };
                    answers.push(HostReply::Done(self.fold(s, done?)?));
                }
            }
        }
        Ok(answers)
    }

    /// Check one call and, if it needs the user, ask. Allocates its
    /// sub-steps. Refusals aren't journaled: they follow from the scope,
    /// the tool and the taint, which a replay sees the same way.
    async fn plan(
        &self,
        s: &mut Serving<'_>,
        call: ToolCall,
        reached: &mut BTreeSet<String>,
    ) -> Result<Planned, StepError> {
        let name = call.name.clone();
        let refuse = |why: String| Ok(Planned::Answer(HostReply::Refused(why)));
        if !s.callable.contains(&name) {
            return refuse(
                self.not_callable(s.scope, &name)
                    .unwrap_or_else(|| "not available".into()),
            );
        }
        let policy = self.tools.policy(s.ctx, &call);
        match policy.effect {
            Effect::Read => {}
            Effect::Write if s.limits.allow_writes => {}
            Effect::Write => {
                return refuse(
                    "this call would be a write; scripts can only read here (`[python] \
                     allow_writes` is off). Call it directly"
                        .into(),
                );
            }
            Effect::Destructive => {
                return refuse(
                    "this call would be destructive; scripts can't make those. Call it directly"
                        .into(),
                );
            }
        }
        if policy.effect == Effect::Write && s.writes >= s.limits.max_writes {
            return refuse(format!(
                "too many writes in one script (the limit is {}); make this one directly",
                s.limits.max_writes
            ));
        }

        // The tainted host limit, as for a direct call.
        let host = self.tools.egress_host(&call);
        let limited = s.egress.limits(s.tainted, host.as_deref())
            && host.as_deref().is_none_or(|h| !reached.contains(h));
        let gate = Gate {
            tainted: s.tainted,
            autonomy: s.autonomy,
        };
        let only_limit = limited && !approval::needs_check(&name, &policy, gate);
        let prompt_host = host.clone().filter(|_| only_limit);
        let policy = if limited { policy.forced(true) } else { policy };
        let key_granted = match self.tools.grant_key(&call) {
            Some(k) if policy.forbidden.is_none() => self.key_granted(s.run, &name, &k)?,
            _ => false,
        };
        let granted = policy.forbidden.is_none()
            && (key_granted
                || self.tools.allow_always(&call)
                    && approval::allowed_by(&self.store.allowlist(&name)?, &call));
        // A standing grant for the host answers the host limit.
        let host_granted = only_limit
            && host.as_deref().is_some_and(|h| {
                self.store
                    .allowlist(&name)
                    .is_ok_and(|l| l.contains(&approval::host_pattern(h)))
            });

        let ask_seq = s.seq + 1;
        let ask_input = json!({
            "in": s.idx,
            "seq": ask_seq,
            "ask": { "name": name, "input": call.input },
        });
        let recorded = self.is_recorded(s, s.idx + ask_seq, &ask_input)?;
        let asks = match approval::check(&name, &policy, granted, gate) {
            Check::Forbid(flag) => {
                tracing::warn!(run = %s.run.id, tool = %name, flag, "refused a forbidden tool called from a script");
                self.ledger_refused(s.run, &call, flag)?;
                return refuse(approval::forbidden_message(flag));
            }
            Check::ReadOnly => return refuse(approval::READONLY_MESSAGE.to_owned()),
            Check::Allow => recorded,
            Check::Ask { .. } => !host_granted || recorded,
        };

        if asks {
            if !recorded && s.asks >= s.limits.max_asks {
                return refuse(format!(
                    "too many approvals in one script (the limit is {}); call this tool directly",
                    s.limits.max_asks
                ));
            }
            s.asks += 1;
            if let Some(why) = self.diverged(s, s.idx + ask_seq, &ask_input)? {
                return Ok(Planned::Answer(HostReply::Abort(why)));
            }
            s.seq = ask_seq;
            let origin = format!(
                "from a python script (call {} of this script)\n{}",
                s.calls,
                crate::tool::truncate_middle(&s.code, SCRIPT_CARD_BYTES)
            );
            let started = Instant::now();
            let asked = self
                .journal
                .step_with(
                    &s.run.id,
                    s.idx + ask_seq,
                    StepKind::PyCall,
                    &ask_input,
                    InFlight::Retry,
                    || async {
                        let (outcome, why) = self
                            .decide(
                                s.run,
                                &call,
                                &policy,
                                gate,
                                s.taint_reason.as_deref(),
                                prompt_host.as_deref(),
                                Some(origin),
                                &s.ctx.cancel,
                            )
                            .await?;
                        Ok(StepOutput::new(Asked { outcome, why }))
                    },
                )
                .await?;
            if !asked.replayed {
                s.host.add_wait(started.elapsed());
            }
            self.emit(AgentEvent::Approval {
                run_id: s.run.id.clone(),
                id: call.id.clone(),
                name: name.clone(),
                outcome: asked.value.outcome,
            });
            let Asked { outcome, why } = asked.value;
            match outcome {
                ApprovalOutcome::Approved | ApprovalOutcome::ApprovedByGrant => {}
                ApprovalOutcome::ApprovedForRun => s.egress.allow_all = true,
                ApprovalOutcome::Deferred => {
                    // The outbox's contract: carry on with whatever doesn't
                    // depend on it. The script sees that it was queued.
                    let queued = json!({
                        "queued": true,
                        "outbox": crate::outbox::deferred_id(&why),
                        "tool": name,
                    });
                    return Ok(Planned::Answer(HostReply::Done(ToolResult::ok(
                        queued.to_string(),
                    ))));
                }
                ApprovalOutcome::Denied | ApprovalOutcome::Timeout => {
                    return refuse(why);
                }
            }
        }
        reached.extend(host.clone());

        s.seq += 1;
        let step = s.idx + s.seq;
        let input = json!({
            "in": s.idx,
            "seq": s.seq,
            "call": { "name": name, "input": call.input },
        });
        if let Some(why) = self.diverged(s, step, &input)? {
            return Ok(Planned::Answer(HostReply::Abort(why)));
        }
        if policy.effect == Effect::Write {
            s.writes += 1;
        }
        let in_flight = if policy.idempotent {
            InFlight::Retry
        } else {
            InFlight::Substitute(StepOutput::new(ToolResult::error(INTERRUPTED_TOOL_MESSAGE)))
        };
        let untrusted = self.tools.untrusted(&name);
        Ok(Planned::Run(Box::new(Prepared {
            call,
            step,
            input,
            in_flight,
            untrusted,
            host,
            effect: policy.effect,
        })))
    }

    /// Run (or replay) one planned call as its sub-step.
    async fn exec(
        &self,
        run: &RunRow,
        scope: Option<&[String]>,
        ctx: &ToolCtx,
        p: Prepared,
    ) -> Result<Executed, StepError> {
        let Prepared {
            call,
            step,
            input,
            in_flight,
            untrusted,
            host,
            effect,
        } = p;
        let result = self
            .journal
            .step_with(
                &run.id,
                step,
                StepKind::PyCall,
                &input,
                in_flight,
                || async {
                    let mut r = self.tools.call_in(scope, ctx, &call).await;
                    r.untrusted |= untrusted;
                    Ok(StepOutput::new(r))
                },
            )
            .await?;
        Ok(Executed {
            call,
            host,
            effect,
            result,
        })
    }

    /// Fold a finished call into the script's state: taint, the hosts it
    /// reached, and the ledger for a change it made.
    fn fold(&self, s: &mut Serving<'_>, e: Executed) -> Result<ToolResult, StepError> {
        let Executed {
            call,
            host,
            effect,
            result,
        } = e;
        if result.value.untrusted {
            s.tainted = true;
            s.taint_reason
                .get_or_insert_with(|| format!("python called {}", call.name));
        }
        if !result.value.content.starts_with(EGRESS_REFUSED) {
            s.egress.hosts.extend(host);
        }
        if effect != Effect::Read && !result.replayed {
            let summary = self.tools.summarize(&call);
            self.ledger_effect(s.run, &call, effect, &summary, result.value.is_error, None)?;
        }
        Ok(result.value)
    }

    /// Whether the journal holds a sub-step at `idx` with exactly `input`.
    fn is_recorded(&self, s: &Serving<'_>, idx: u32, input: &Value) -> Result<bool, StepError> {
        let hash = input_hash(StepKind::PyCall, input)?;
        Ok(self
            .store
            .step(&s.run.id, idx)?
            .is_some_and(|r| r.kind == StepKind::PyCall.as_str() && r.input_hash == hash))
    }

    /// A re-run script that asks for something other than what the journal
    /// holds at `idx` has diverged (spec §4.2). When what the journal holds
    /// from there on includes a write that already ran (or was queued), the
    /// script must not carry on: the answer says which, and the `python`
    /// step fails with it. Otherwise `None`, and the journal truncates and
    /// the script continues live, as for any divergence (repeating a read is
    /// harmless).
    fn diverged(
        &self,
        s: &Serving<'_>,
        idx: u32,
        input: &Value,
    ) -> Result<Option<String>, StepError> {
        let Some(row) = self.store.step(&s.run.id, idx)? else {
            return Ok(None);
        };
        if row.input_hash == input_hash(StepKind::PyCall, input)? {
            return Ok(None);
        }
        let mut ran = Vec::new();
        for j in idx.. {
            let Some(row) = self.store.step(&s.run.id, j)? else {
                break;
            };
            if row.kind != StepKind::PyCall.as_str() || row.status != "completed" {
                continue;
            }
            let Ok(old) = serde_json::from_str::<Value>(&row.input_json) else {
                continue;
            };
            if old["in"].as_u64() != Some(u64::from(s.idx)) {
                break;
            }
            if let Some(c) = old.get("call") {
                let call = ToolCall {
                    id: String::new(),
                    name: c["name"].as_str().unwrap_or_default().to_owned(),
                    input: c["input"].clone(),
                };
                if self.tools.policy(s.ctx, &call).effect != Effect::Read {
                    ran.push(self.tools.summarize(&call));
                }
            } else if let Some(c) = old.get("ask") {
                let queued = row
                    .output_json
                    .as_deref()
                    .and_then(|o| serde_json::from_str::<Asked>(o).ok())
                    .is_some_and(|a| a.outcome == ApprovalOutcome::Deferred);
                if queued {
                    let name = c["name"].as_str().unwrap_or_default();
                    ran.push(format!("{name} (queued in the outbox)"));
                }
            }
        }
        if ran.is_empty() {
            return Ok(None);
        }
        Ok(Some(format!(
            "script changed on resume after these calls ran: {}. Writes are never repeated; check \
             what they did before running anything again",
            ran.join(", ")
        )))
    }

    /// What a journaled script sub-step at `idx` tells the loop that comes
    /// after the script's step (live and on replay): the host a network read
    /// reached, and an "all sites for this run" answer.
    pub(super) fn script_substep(&self, run: &str, idx: u32) -> Result<SubStep, StepError> {
        let Some(row) = self.store.step(run, idx)? else {
            return Ok(SubStep::default());
        };
        let input: Value = serde_json::from_str(&row.input_json)?;
        if input.get("ask").is_some() {
            let allow_all_egress = row
                .output_json
                .as_deref()
                .and_then(|o| serde_json::from_str::<Asked>(o).ok())
                .is_some_and(|a| a.outcome == ApprovalOutcome::ApprovedForRun);
            return Ok(SubStep {
                host: None,
                allow_all_egress,
            });
        }
        let Some(c) = input.get("call") else {
            return Ok(SubStep::default()); // the env sub-step
        };
        let refused = row
            .output_json
            .as_deref()
            .and_then(|o| serde_json::from_str::<ToolResult>(o).ok())
            .is_none_or(|r| r.content.starts_with(EGRESS_REFUSED));
        if refused {
            return Ok(SubStep::default());
        }
        let call = ToolCall {
            id: String::new(),
            name: c["name"].as_str().unwrap_or_default().to_owned(),
            input: c["input"].clone(),
        };
        Ok(SubStep {
            host: self.tools.egress_host(&call),
            allow_all_egress: false,
        })
    }
}
