//! Serving a script's tool calls (docs/python-tool-spec.md §3.3, §4): each
//! is a journaled `py_call` sub-step after the script's own `tool` step at
//! `idx`, checked like a top-level call. Sub-step `idx + 1` records the
//! script's clock and seed; calls follow in the order the script makes them,
//! so a script re-run after a crash is answered from the journal.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;

use serde_json::json;
use tern_store::RunRow;

use super::{Agent, INTERRUPTED_TOOL_MESSAGE};
use crate::approval::{self, Check};
use crate::journal::{InFlight, StepError, StepKind, StepOutput};
use crate::script::{self, HostReply, HostRequest, ScriptEnv, ScriptHost};
use crate::tool::{Effect, ToolCtx, ToolResult};
use crate::types::ToolCall;
use crate::types::ToolSpec;

/// The loop's state while one script runs.
struct Serving<'a> {
    run: &'a RunRow,
    /// The script's own step.
    idx: u32,
    call_id: &'a str,
    scope: Option<&'a [String]>,
    ctx: &'a ToolCtx,
    callable: HashSet<String>,
    /// Sub-steps used so far.
    seq: u32,
    /// The run's taint, updated by the script's own untrusted results.
    tainted: bool,
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
        if script::callable_in_scripts(name, tool.effect()) {
            return None;
        }
        Some(if tool.effect() == Effect::Read {
            "this tool can't be used from a script; call it directly".into()
        } else {
            format!(
                "{} tool; scripts can only call read-only tools; call it directly",
                tool.effect()
            )
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
        tainted: bool,
        fut: F,
    ) -> Result<T, StepError>
    where
        F: Future<Output = T>,
    {
        if call.name != script::PYTHON {
            return Ok(fut.await);
        }
        let (callable, blocked) = self.script_tools(scope);
        let mut s = Serving {
            run,
            idx,
            call_id: &call.id,
            scope,
            ctx,
            callable: callable.iter().map(|t| t.name.clone()).collect(),
            seq: 0,
            tainted,
        };
        let (host, mut rx) = ScriptHost::channel(callable, blocked);
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
            HostRequest::Call { name, input, reply } => {
                let call = ToolCall {
                    id: format!("{}.{}", s.call_id, s.seq + 1),
                    name,
                    input,
                };
                let answer = match self.refusal(s, &call) {
                    Some(why) => HostReply::Refused(why),
                    None => HostReply::Done(self.py_call(s, call).await?),
                };
                let _ = reply.send(answer);
            }
        }
        Ok(())
    }

    /// Why a script can't make `call`, or `None` if it may. Refusals aren't
    /// journaled: they follow from the scope, the tool and the taint, which
    /// a replay sees the same way.
    fn refusal(&self, s: &Serving<'_>, call: &ToolCall) -> Option<String> {
        let name = &call.name;
        if !s.callable.contains(name) {
            return Some(
                self.not_callable(s.scope, name)
                    .unwrap_or_else(|| "not available".into()),
            );
        }
        let policy = self.tools.policy(s.ctx, call);
        if policy.effect != Effect::Read {
            return Some(format!(
                "this call would be a {}; scripts can only read. Call it directly",
                policy.effect
            ));
        }
        match approval::check(name, &policy, false, s.tainted) {
            Check::Allow => None,
            Check::Ask { .. } => Some(
                "this call needs the user's approval, which a script can't ask for yet; call it \
                 directly"
                    .into(),
            ),
            Check::Forbid(flag) => Some(approval::forbidden_message(flag)),
        }
    }

    /// Run (or replay) one call as the next sub-step.
    async fn py_call(&self, s: &mut Serving<'_>, call: ToolCall) -> Result<ToolResult, StepError> {
        s.seq += 1;
        let input = json!({
            "in": s.idx,
            "seq": s.seq,
            "call": { "name": call.name, "input": call.input },
        });
        let in_flight = if self.tools.policy(s.ctx, &call).idempotent {
            InFlight::Retry
        } else {
            InFlight::Substitute(StepOutput::new(ToolResult::error(INTERRUPTED_TOOL_MESSAGE)))
        };
        let untrusted = self.tools.untrusted(&call.name);
        let (scope, ctx) = (s.scope, s.ctx);
        let result = self
            .journal
            .step_with(
                &s.run.id,
                s.idx + s.seq,
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
        s.tainted |= result.value.untrusted;
        Ok(result.value)
    }
}
