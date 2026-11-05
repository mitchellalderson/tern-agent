//! The sub-agent host (docs/subagents-design.md §1.3–§2.4): creates child
//! runs for `agent_spawn`, hands them to a [`RunDriver`], delivers their
//! results to the parent through `finished` mail, and carries questions,
//! answers and steering messages between runs of a tree (`agent_ask_parent`,
//! `agent_send`, `POST /v1/runs/{id}/message`).
//!
//! Lives with `Runtime` rather than the daemon, so sub-agents work wherever
//! runs are driven: the daemon drives children through its `Manager`, the
//! embedded TUI and `tern run` through [`LocalDriver`].

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tern_core::agent::{Agent, RunOutcome, RunStatus, new_id};
use tern_core::config::{ModelRef, SubagentsConfig};
use tern_core::events::AgentEvent;
use tern_core::journal::{Journal, StepError};
use tern_core::subagent::{
    self, AGENT_TOOLS, ASK_PREFIX, AskRequest, CHILD_EXCLUDED_TOOLS, CHILD_ONLY_TOOLS,
    CancelRequest, ChildRun, FINISHED_PREFIX, Finished, Note, Question, READ_ONLY_TOOLS,
    ResultRequest, SendRequest, SpawnRequest, SubagentHost, SubagentLimits, ToolsArg, WaitMode,
    WaitRequest,
};
use tern_core::tool::{ToolCtx, ToolRegistry, consume_mail, progress_sink};
use tern_core::types::Usage;
use tern_core::types::{ContentBlock, Message};
use tern_store::{MailRow, NewMail, NewRun, RunRow, Store, now_ms};
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;

/// How often waits re-check the store, in case a wakeup was missed (or the
/// mail came from another process).
const POLL: Duration = Duration::from_millis(250);
const DEFAULT_WAIT_SECS: u64 = 900;
const PARENT_ENDED: &str = "cancelled: the parent run ended";
const USER_CANCELLED: &str = "cancelled by the user";

/// Whoever drives runs in this process.
pub trait RunDriver: Send + Sync {
    /// Drive `run_id` in the background (`resume` for a run that was
    /// already started or queued). False if it is already being driven.
    fn drive(&self, run_id: &str, resume: bool) -> bool;
    /// Cancel `run_id` if it is being driven here.
    fn cancel(&self, run_id: &str) -> bool;
    fn is_active(&self, run_id: &str) -> bool;
}

/// Drives child runs on tokio tasks: the embedded TUI and `tern run`.
pub struct LocalDriver {
    agent: Weak<Agent>,
    active: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl LocalDriver {
    pub fn new(agent: &Arc<Agent>) -> Self {
        Self {
            agent: Arc::downgrade(agent),
            active: Arc::default(),
        }
    }
}

impl RunDriver for LocalDriver {
    fn drive(&self, run_id: &str, resume: bool) -> bool {
        let (Ok(handle), Some(agent)) =
            (tokio::runtime::Handle::try_current(), self.agent.upgrade())
        else {
            tracing::warn!(run = run_id, "no runtime to drive a sub-agent on");
            return false;
        };
        let cancel = CancellationToken::new();
        {
            let Ok(mut active) = self.active.lock() else {
                return false;
            };
            if active.contains_key(run_id) {
                return false;
            }
            active.insert(run_id.to_owned(), cancel.clone());
        }
        let (active, id) = (self.active.clone(), run_id.to_owned());
        handle.spawn(async move {
            let result = if resume {
                agent.resume(&id, cancel).await
            } else {
                agent.drive(&id, cancel).await
            };
            if let Err(e) = result {
                tracing::error!(run = %id, error = %e, "sub-agent could not be driven");
            }
            if let Ok(mut a) = active.lock() {
                a.remove(&id);
            }
        });
        true
    }

    fn cancel(&self, run_id: &str) -> bool {
        self.active
            .lock()
            .ok()
            .and_then(|a| a.get(run_id).map(CancellationToken::cancel))
            .is_some()
    }

    fn is_active(&self, run_id: &str) -> bool {
        self.active.lock().is_ok_and(|a| a.contains_key(run_id))
    }
}

pub struct Subagents {
    store: Store,
    journal: Journal,
    events: broadcast::Sender<AgentEvent>,
    /// Weak: the registry holds the `agent_*` tools, which hold this host.
    tools: Weak<ToolRegistry>,
    config: SubagentsConfig,
    limits: SubagentLimits,
    /// The live config, so providers added after startup (e.g. from the
    /// desktop setup) are usable. Any provider is accepted with the mock.
    live: Arc<RwLock<tern_core::Config>>,
    mock: bool,
    driver: RwLock<Option<Arc<dyn RunDriver>>>,
    /// Builds a child's system prompt from its label and depth.
    prompt: OnceLock<PromptFn>,
    /// Woken when mail is posted.
    notify: Notify,
    /// Why runs were cancelled, until their drive ends and records it.
    reasons: Mutex<HashMap<String, String>>,
    /// Serializes "is there room?" decisions (spawning and dequeueing).
    gate: Mutex<()>,
}

/// A sub-agent's system prompt, from its label, depth, the agent it works
/// as (its parent's, or a delegate's) and, for a delegate, the caller's
/// name.
pub type PromptFn = Box<dyn Fn(&str, u32, &str, Option<&str>) -> String + Send + Sync>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Subagents {
    pub fn new(
        store: Store,
        journal: Journal,
        events: broadcast::Sender<AgentEvent>,
        tools: Weak<ToolRegistry>,
        config: &tern_core::Config,
        live: Arc<RwLock<tern_core::Config>>,
        mock: bool,
    ) -> Self {
        Self {
            store,
            journal,
            events,
            tools,
            limits: SubagentLimits::from(&config.subagents),
            config: config.subagents.clone(),
            live,
            mock,
            driver: RwLock::new(None),
            prompt: OnceLock::new(),
            notify: Notify::new(),
            reasons: Mutex::new(HashMap::new()),
            gate: Mutex::new(()),
        }
    }

    /// Replace the driver (the daemon swaps in its `Manager`).
    pub fn set_driver(&self, driver: Arc<dyn RunDriver>) {
        if let Ok(mut d) = self.driver.write() {
            *d = Some(driver);
        }
    }

    /// Set how children's system prompts are built. Without it they get
    /// their parent's.
    pub fn set_prompt(&self, prompt: PromptFn) {
        let _ = self.prompt.set(prompt);
    }

    fn driver(&self) -> Option<Arc<dyn RunDriver>> {
        self.driver.read().ok().and_then(|d| d.clone())
    }

    fn run(&self, id: &str) -> Result<RunRow, String> {
        self.store
            .run(id)
            .map_err(err)?
            .ok_or_else(|| format!("run {id} not found"))
    }

    /// Startup: every child that ended without reporting to its parent (a
    /// crash between the two writes) reports now. Safe to repeat.
    pub fn reconcile(&self) -> anyhow::Result<()> {
        for child in self.store.child_runs_without_mail(FINISHED_PREFIX)? {
            if is_ended(&child) {
                tracing::info!(run = %child.id, "posting a lost sub-agent report");
                self.report(&child, None)?;
            }
        }
        Ok(())
    }

    /// Start queued children while there is room, oldest first. Children
    /// whose parent is no longer running are cancelled instead.
    pub fn kick(&self) {
        if let Err(e) = self.try_kick() {
            tracing::warn!(error = %e, "starting queued sub-agents failed");
        }
    }

    fn try_kick(&self) -> anyhow::Result<()> {
        let mut start = Vec::new();
        let mut orphans = Vec::new();
        {
            let _g = self
                .gate
                .lock()
                .map_err(|_| anyhow::anyhow!("lock poisoned"))?;
            for child in self.store.child_runs_with_status("queued")? {
                let Some(parent_id) = child.parent_run_id.as_deref() else {
                    continue;
                };
                let parent = self.store.run(parent_id)?;
                if parent.as_ref().is_none_or(|p| p.status != "running") {
                    orphans.push(child);
                    continue;
                }
                if self.has_room(parent_id)? {
                    self.store.set_run_status(&child.id, "running", None)?;
                    start.push(child.id);
                }
            }
        }
        for child in orphans {
            self.stop_undriven(&child, PARENT_ENDED)?;
        }
        for id in start {
            tracing::info!(run = %id, "starting queued sub-agent");
            self.start(&id, true);
        }
        Ok(())
    }

    /// Room for one more running child under `parent` (and globally).
    fn has_room(&self, parent: &str) -> anyhow::Result<bool> {
        let mine = self
            .store
            .children(parent)?
            .iter()
            .filter(|c| c.status == "running")
            .count();
        let all = self.store.child_runs_with_status("running")?.len();
        Ok(mine < self.limits.max_concurrent as usize
            && all < self.limits.max_concurrent_global as usize)
    }

    fn start(&self, run_id: &str, resume: bool) {
        match self.driver() {
            Some(d) => {
                d.drive(run_id, resume);
            }
            None => tracing::warn!(run = run_id, "no driver for sub-agents"),
        }
    }

    /// Make sure a child the parent is waiting on can make progress: drive
    /// it if nobody is (after a crash, or in a process that didn't start
    /// it), start the queue, or post a lost report.
    fn ensure_driven(&self, child: &RunRow) -> anyhow::Result<()> {
        match child.status.as_str() {
            "queued" => self.kick(),
            "running" => self.resume_if_idle(child),
            "interrupted" if !subagent::was_cancelled(child) => self.resume_if_idle(child),
            _ => {
                if self.finished_mail(&child.id)?.is_none() {
                    self.report(child, None)?;
                }
            }
        }
        Ok(())
    }

    fn resume_if_idle(&self, child: &RunRow) {
        if let Some(d) = self.driver()
            && !d.is_active(&child.id)
            && d.drive(&child.id, true)
        {
            tracing::info!(run = %child.id, "resuming a sub-agent nobody was driving");
        }
    }

    fn finished_mail(&self, child: &str) -> anyhow::Result<Option<MailRow>> {
        Ok(self
            .store
            .mail_by_key(&format!("{FINISHED_PREFIX}{child}"))?)
    }

    /// Post `finished` mail for an ended child (from its outcome when there
    /// is one, else from the store).
    fn report(&self, child: &RunRow, outcome: Option<&RunOutcome>) -> anyhow::Result<()> {
        let Some(parent) = child.parent_run_id.as_deref() else {
            return Ok(());
        };
        let row = self.store.run(&child.id)?.unwrap_or_else(|| child.clone());
        let text = match outcome {
            Some(o) => o.final_text.clone(),
            None => final_text(&self.store, &row)?,
        };
        let status = subagent::finished_status(&row.status, &text);
        let (i, o, cr, cw) = self.store.run_usage(&row.id)?;
        // Only a bounded report reaches the parent's context; the full text
        // stays in the child's session (`agent_result` reads it).
        let label = row.label.clone().unwrap_or_default();
        let (result, result_chars) = if text.trim().is_empty() {
            (None, None)
        } else {
            let (t, total) = subagent::cap_report(&text, self.limits.max_result_chars, &label);
            (Some(t), total)
        };
        let body = Finished {
            agent: label,
            run_id: row.id.clone(),
            status: status.into(),
            result,
            result_chars,
            error: row.error.clone(),
            iterations: self.store.step_count(&row.id, "llm")?,
            tokens: i + o + cr + cw,
        };
        let posted = self.store.post_mail(&NewMail {
            to_run_id: parent,
            from_run_id: Some(&row.id),
            kind: "finished",
            body: &serde_json::to_string(&body)?,
            dedupe_key: &format!("{FINISHED_PREFIX}{}", row.id),
        })?;
        if posted {
            tracing::info!(run = %row.id, parent, status, "sub-agent reported");
        }
        self.notify.notify_waiters();
        Ok(())
    }

    /// Cancel `child` and its subtree with `reason`.
    fn cancel_child(&self, child: &RunRow, reason: &str) -> anyhow::Result<()> {
        if is_ended(child) {
            return Ok(());
        }
        if let Ok(mut r) = self.reasons.lock() {
            r.insert(child.id.clone(), reason.to_owned());
        }
        let driven = self.driver().is_some_and(|d| d.cancel(&child.id));
        if !driven {
            // Queued, or nobody is driving it: there's no loop to stop.
            return self.stop_undriven(child, reason);
        }
        // Recorded now as well as when its drive ends, so a process that
        // exits first (Ctrl+C in `tern run`) doesn't leave it resumable.
        // Its report and its own children follow when the drive ends.
        self.store
            .set_run_status(&child.id, RunStatus::Interrupted.as_str(), Some(reason))?;
        Ok(())
    }

    fn stop_undriven(&self, child: &RunRow, reason: &str) -> anyhow::Result<()> {
        if let Ok(mut r) = self.reasons.lock() {
            r.remove(&child.id);
        }
        self.store
            .set_run_status(&child.id, RunStatus::Interrupted.as_str(), Some(reason))?;
        for grandchild in self.store.children(&child.id)? {
            self.cancel_child(&grandchild, PARENT_ENDED)?;
        }
        self.report(child, None)
    }

    /// Cancel a sub-agent run (from the API). False if `run_id` isn't one.
    pub fn cancel_run(&self, run_id: &str, reason: &str) -> bool {
        match self.store.run(run_id) {
            Ok(Some(run)) if run.parent_run_id.is_some() && !is_ended(&run) => {
                self.cancel_child(&run, reason).is_ok()
            }
            _ => false,
        }
    }

    /// The child's model: `requested`, else a delegate's own model (it runs
    /// as that agent), else the parent agent's `subagent_model`, the
    /// configured default, the parent's.
    fn pick_model(
        &self,
        requested: Option<&str>,
        parent: &RunRow,
        delegate: Option<&tern_store::AgentRow>,
    ) -> Result<String, String> {
        let name = requested
            .map(|m| {
                self.config
                    .models
                    .get(m)
                    .map_or_else(|| m.to_owned(), |c| c.primary().to_owned())
            })
            .or_else(|| {
                let m = delegate?.model.as_deref()?;
                tern_core::config::ModelChain::from_stored(m).map(|c| c.primary().to_owned())
            })
            // The parent agent's `subagent_model` (agent profiles §6.3).
            .or_else(|| {
                if delegate.is_some() {
                    return None;
                }
                let agent = self.store.session_agent(&parent.session_id).ok()?;
                let m = self.store.agent(&agent).ok()??.subagent_model?;
                tern_core::config::ModelChain::from_stored(&m).map(|c| c.primary().to_owned())
            })
            .or_else(|| self.config.default_model.as_deref().map(str::to_owned))
            .or_else(|| parent.model.clone())
            .ok_or("no model to run the sub-agent with")?;
        let aliases = || {
            let names: Vec<&str> = self.config.models.keys().map(String::as_str).collect();
            if names.is_empty() {
                "none configured".to_owned()
            } else {
                names.join(", ")
            }
        };
        let m: ModelRef = name.parse().map_err(|_| {
            format!(
                "unknown model {name:?}: use an alias ({}) or provider:model",
                aliases()
            )
        })?;
        let configured = self
            .live
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .providers
            .contains_key(&m.provider);
        if !self.mock && !configured {
            return Err(format!(
                "provider {:?} is not configured; use an alias ({}) or a configured provider",
                m.provider,
                aliases()
            ));
        }
        Ok(m.to_string())
    }

    /// The agent `caller` hands work to as `name` (id, @name or name), if
    /// it may (agent profiles §6.1); the inner error is for the model.
    /// "Where you are" for a child of `parent`: whom it works for, where,
    /// and that its report goes back (frozen with its prompt).
    fn where_section(
        &self,
        parent: &tern_store::RunRow,
        caller: Option<&str>,
        can_post: bool,
    ) -> anyhow::Result<String> {
        use tern_core::channels::Place;
        let place = match self.store.session_channel(&parent.session_id)? {
            Some(c) if c.kind == "topic" => Some(
                Place::Topic {
                    slug: &c.slug,
                    title: &c.title,
                }
                .name(),
            ),
            Some(c) if c.id == tern_store::HOME => Some(Place::Home.name()),
            Some(c) => Some(match self.store.agent(c.lead())? {
                Some(a) => Place::Dm(&a.name).name(),
                None => c.title.clone(),
            }),
            None => None,
        };
        let caller = match parent
            .label
            .as_deref()
            .filter(|_| parent.parent_run_id.is_some())
        {
            Some(label) => label.to_owned(),
            None => caller
                .unwrap_or(tern_core::channels::author::AGENT)
                .to_owned(),
        };
        Ok(tern_core::channels::subagent_where_section(
            &caller,
            place.as_deref(),
            can_post,
        ))
    }

    fn delegate(
        &self,
        caller: &str,
        name: &str,
    ) -> anyhow::Result<Result<tern_store::AgentRow, String>> {
        let Some(me) = self.store.agent(caller)? else {
            return Ok(Err(format!("agent {caller} not found")));
        };
        let wanted = name.trim().trim_start_matches('@');
        let agents = self.store.agents(false)?;
        let Some(target) = agents
            .iter()
            .find(|a| a.id == wanted || a.slug == wanted || a.name.eq_ignore_ascii_case(wanted))
        else {
            let names: Vec<String> = agents
                .iter()
                .filter(|a| me.may_delegate_to(a))
                .map(|a| a.slug.clone())
                .collect();
            return Ok(Err(format!(
                "no agent {wanted:?}; you may delegate to: {}",
                if names.is_empty() {
                    "none".into()
                } else {
                    names.join(", ")
                }
            )));
        };
        if target.id == me.id {
            return Ok(Err(
                "that's you: leave out `agent` to start a sub-agent of your own".into(),
            ));
        }
        if !me.may_delegate_to(target) {
            return Ok(Err(format!(
                "{} can't hand work to {}: it isn't in {}'s delegates",
                me.name, target.name, me.name
            )));
        }
        Ok(Ok(target.clone()))
    }

    /// `a`'s tool scope as a run freezes it; `None` = every tool.
    fn frozen_scope(&self, a: &tern_store::AgentRow) -> Option<Vec<String>> {
        let scope = tern_core::agents::ToolScope::parse(a.tools.as_deref()?).unwrap_or_else(|_| {
            tern_core::agents::ToolScope {
                builtins: tern_core::agents::Pick::ReadOnly,
                connectors: tern_core::agents::Pick::Names(Vec::new()),
                mcp: tern_core::agents::Pick::Names(Vec::new()),
                skills: tern_core::agents::Pick::Names(Vec::new()),
            }
        });
        let registry = self.tools.upgrade()?;
        Some(scope.freeze(&registry.builtin_names()))
    }

    /// The tool names a child at `depth` may use: never more than `limit`
    /// (its parent's scope, or the delegate's own; `None` = every tool).
    fn scope_for(
        &self,
        tools: Option<&ToolsArg>,
        depth: u32,
        limit: Option<Vec<String>>,
    ) -> Result<Vec<String>, String> {
        let registry = self.tools.upgrade().ok_or("tools are gone")?;
        let parent_scope = limit;
        let all: Vec<String> = registry
            .names()
            .into_iter()
            .filter(|n| {
                parent_scope.is_none()
                    || tern_core::tool::in_scope(parent_scope.as_deref(), n)
                    || CHILD_ONLY_TOOLS.contains(&n.as_str())
                    || AGENT_TOOLS.contains(&n.as_str())
            })
            .collect();
        let mut names: Vec<String> = match tools {
            None => all.clone(),
            Some(ToolsArg::Preset(p)) if p == "all" => all.clone(),
            Some(ToolsArg::Preset(p)) if p == "read_only" => all
                .iter()
                .filter(|n| READ_ONLY_TOOLS.contains(&n.as_str()))
                .cloned()
                .collect(),
            Some(ToolsArg::Preset(p)) => {
                return Err(format!(
                    "tools must be \"all\", \"read_only\" or a list of tool names, not {p:?}"
                ));
            }
            Some(ToolsArg::Names(list)) => {
                if let Some(bad) = list.iter().find(|n| !all.contains(n)) {
                    return Err(format!("unknown tool {bad:?}"));
                }
                list.clone()
            }
        };
        names.retain(|n| {
            let n = n.as_str();
            !CHILD_EXCLUDED_TOOLS.contains(&n)
                && !AGENT_TOOLS.contains(&n)
                && !CHILD_ONLY_TOOLS.contains(&n)
        });
        let parent_side: &[&str] = if depth < self.limits.max_depth {
            AGENT_TOOLS
        } else {
            &[]
        };
        names.extend(
            parent_side
                .iter()
                .chain(CHILD_ONLY_TOOLS)
                .map(|t| (*t).to_owned())
                .filter(|t| all.contains(t)),
        );
        names.sort();
        names.dedup();
        Ok(names)
    }

    fn handle(&self, child: &RunRow) -> anyhow::Result<Value> {
        Ok(json!({
            "agent": child.label,
            "run_id": child.id,
            "status": self.status_of(child)?,
            "model": child.model,
        }))
    }

    /// A child's status as its parent sees it.
    fn status_of(&self, child: &RunRow) -> anyhow::Result<String> {
        if let Some(m) = self.finished_mail(&child.id)?
            && let Ok(f) = serde_json::from_str::<Finished>(&m.body)
        {
            return Ok(f.status);
        }
        Ok(match child.status.as_str() {
            "queued" => "queued".into(),
            "running" | "interrupted" => "running".into(),
            other => subagent::finished_status(other, "").into(),
        })
    }

    /// What a still-running child is doing, from its newest journal step.
    fn activity(&self, child: &RunRow) -> Option<String> {
        let (kind, status, input) = self.store.last_step(&child.id).ok()??;
        if status != "started" {
            return Some("Thinking".into());
        }
        Some(match kind.as_str() {
            "tool" => {
                let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
                format!("Running {}", v["name"].as_str().unwrap_or("a tool"))
            }
            "approval" => "Waiting for approval".into(),
            "inbox" => "Waiting on sub-agents".into(),
            "compaction" => "Summarizing context".into(),
            _ => "Thinking".into(),
        })
    }

    fn live_entry(&self, child: &RunRow) -> anyhow::Result<Value> {
        let mut v = json!({
            "agent": child.label,
            "status": self.status_of(child)?,
            "run_id": child.id,
            "elapsed_secs": (now_ms() - child.started_at).max(0) / 1000,
        });
        if let Some((mail, q)) = pending_question(&self.store, child)? {
            v["status"] = "question".into();
            v["question_id"] = subagent::question_id(mail.id).into();
            v["question"] = q.question.into();
            if let Some(r) = q.recommendation {
                v["recommendation"] = r.into();
            }
        } else if child.status == "running"
            && let Some(a) = self.activity(child)
        {
            v["activity"] = a.into();
        }
        Ok(v)
    }

    fn report_entry(mail: &MailRow) -> Value {
        match serde_json::from_str::<Finished>(&mail.body) {
            Ok(f) => {
                let mut v = json!({
                    "agent": f.agent,
                    "status": f.status,
                    "iterations": f.iterations,
                    "tokens": f.tokens,
                    "run_id": f.run_id,
                });
                if let Some(r) = f.result {
                    v["result"] = r.into();
                }
                if let Some(e) = f.error {
                    v["error"] = e.into();
                }
                v
            }
            Err(_) => json!({ "raw": mail.body }),
        }
    }

    async fn sleep_or_wake(&self, cancel: &CancellationToken) -> Result<(), StepError> {
        tokio::select! {
            () = self.notify.notified() => Ok(()),
            () = tokio::time::sleep(POLL) => Ok(()),
            () = cancel.cancelled() => Err(StepError::Cancelled),
        }
    }

    fn try_spawn(&self, ctx: &ToolCtx, call_id: &str, req: SpawnRequest) -> anyhow::Result<Value> {
        let parent = self.run(&ctx.run_id).map_err(anyhow::Error::msg)?;
        // Re-executed after a crash: the child exists already.
        if let Some(child) = self.store.child_by_call(&parent.id, call_id)? {
            return self.handle(&child);
        }
        let fail = |m: String| Err(anyhow::Error::msg(m));
        // Narrowing to a subfolder: only ever deeper inside the caller's own
        // handle. Refused here, before anything is created.
        let narrowed = match req.subdir.as_deref() {
            Some(sub) => Some(
                ctx.scope
                    .narrow(sub)
                    .map_err(|e| anyhow::Error::msg(ctx.scope.fail("agent_spawn", sub, e)))?,
            ),
            None => None,
        };
        if !valid_label(&req.label) {
            return fail(format!(
                "invalid label {:?}: use lowercase letters, digits and dashes (up to 32), e.g. \"pricing-research\"",
                req.label
            ));
        }
        if req.task.trim().is_empty() {
            return fail("task is empty: give the sub-agent its complete brief".into());
        }
        let depth = parent.depth + 1;
        if depth > self.limits.max_depth {
            return fail("sub-agents at this depth can't start sub-agents of their own".into());
        }
        let kids = self.store.children(&parent.id)?;
        if kids
            .iter()
            .any(|k| k.label.as_deref() == Some(req.label.as_str()))
        {
            return fail(format!(
                "label {:?} is taken by another of your sub-agents; pick another",
                req.label
            ));
        }
        if kids.len() >= self.limits.max_children as usize {
            return fail(format!(
                "limit reached: at most {} sub-agents per run",
                self.limits.max_children
            ));
        }
        // Delegating to another profile (agent profiles §6.1): the child
        // runs as that agent, still as this run's sub-agent.
        let caller = self.store.session_agent(&parent.session_id)?;
        let delegate = match req.agent.as_deref() {
            Some(name) => match self.delegate(&caller, name)? {
                Ok(a) => Some(a),
                Err(e) => return fail(e),
            },
            None => None,
        };
        let model = self
            .pick_model(req.model.as_deref(), &parent, delegate.as_ref())
            .map_err(anyhow::Error::msg)?;
        let limit = match &delegate {
            Some(a) => self.frozen_scope(a),
            None => parent
                .tool_scope
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok()),
        };
        let mut scope = self
            .scope_for(req.tools.as_ref(), depth, limit)
            .map_err(anyhow::Error::msg)?;
        // A posting child gets the tool to post with, whatever its preset.
        if req.post && !scope.iter().any(|t| t == "channel_post") {
            scope.push("channel_post".into());
        }
        let limits = child_limits(&self.limits, &req);
        let (max_iterations, budget) = (limits.max_iterations, limits.budget_tokens);

        let session_id = new_id();
        let run_id = new_id();
        let caller_name = self.store.agent(&caller)?.map(|a| a.name);
        let system = match (self.prompt.get(), &delegate) {
            (Some(build), Some(a)) => build(&req.label, depth, &a.id, caller_name.as_deref()),
            (Some(build), None) => build(&req.label, depth, &caller, None),
            (None, _) => parent.system_prompt.clone().unwrap_or_default(),
        };
        let system = format!(
            "{system}\n\n{}{}",
            limits_section(max_iterations, budget),
            self.where_section(&parent, caller_name.as_deref(), req.post)?
        );
        let queued = {
            let _g = self
                .gate
                .lock()
                .map_err(|_| anyhow::anyhow!("lock poisoned"))?;
            let queued = !self.has_room(&parent.id)?;
            self.store.create_session_with_ref(
                &session_id,
                subagent::SESSION_SOURCE,
                &parent.id,
                Some(&model),
            )?;
            let input = self
                .journal
                .to_row(&Message::user_text(req.task.as_str()))?;
            self.store.create_run(
                &NewRun {
                    id: &run_id,
                    session_id: &session_id,
                    trigger: "subagent",
                    model: &model,
                    system_prompt: &system,
                    parent_run_id: Some(&parent.id),
                    parent_call_id: Some(call_id),
                    label: Some(&req.label),
                    depth,
                    tool_scope: Some(&serde_json::to_string(&scope)?),
                    budget_tokens: budget,
                    max_iterations: Some(max_iterations),
                    queued,
                    // Inherited from a tainted parent by the store, as are
                    // its agent and memory scope; a delegate has its own.
                    taint: None,
                    memory_scope: delegate.as_ref().map(|a| {
                        if a.private_memory() {
                            "private"
                        } else {
                            "shared"
                        }
                    }),
                },
                &input,
            )?;
            if let Some(a) = &delegate {
                // Its session is the delegate's, in the caller's channel.
                self.store.set_session_agent(&session_id, &a.id)?;
                // It keeps its caller's autonomy (copied by the store),
                // lowered to its own ceiling if that is stricter.
                // It reaches `code` only if the parent's run does, or its own
                // agent opted in (and its ceiling allows): never because the
                // parent was more permissive than the child's ceiling.
                use tern_core::approval::Autonomy;
                let inherited = Autonomy::of_run(parent.autonomy.as_deref());
                let level = tern_core::approval::delegate_autonomy(
                    inherited,
                    a.autonomy.as_deref().and_then(Autonomy::parse),
                    a.code_auto,
                );
                if level != inherited {
                    self.store.set_run_autonomy(&run_id, level.as_str())?;
                }
            }
            if let Some(n) = &narrowed {
                self.store.set_run_workspace(
                    &run_id,
                    n.id(),
                    &n.root().to_string_lossy(),
                    n.subdir().map(|s| s.to_string_lossy()).as_deref(),
                )?;
            }
            if req.post {
                self.store.set_run_can_post(&run_id)?;
            }
            queued
        };
        let status = if queued { "queued" } else { "running" };
        tracing::info!(run = %run_id, parent = %parent.id, label = %req.label, agent = ?delegate.as_ref().map(|a| &a.id), status, "sub-agent spawned");
        // Announced before it starts, so its run_started comes after.
        let _ = self.events.send(AgentEvent::SubagentSpawned {
            run_id: parent.id.clone(),
            call_id: call_id.to_owned(),
            child_run_id: run_id.clone(),
            child_session_id: session_id,
            label: req.label.clone(),
            task: req.task.chars().take(subagent::TASK_PREVIEW).collect(),
            model: model.clone(),
            status: status.into(),
            agent: delegate.as_ref().map(|a| a.id.clone()),
        });
        if !queued {
            self.start(&run_id, false);
        }
        let mut out = json!({
            "agent": req.label, "run_id": run_id, "status": status, "model": model,
            "max_iterations": max_iterations, "budget_tokens": budget,
        });
        if !limits.lowered.is_empty() {
            out["note"] = json!(format!(
                "lowered to the configured ceiling: {}",
                limits.lowered.join(", ")
            ));
        }
        Ok(out)
    }

    async fn try_wait(&self, ctx: &ToolCtx, req: WaitRequest) -> anyhow::Result<Value> {
        let kids = self.store.children(&ctx.run_id)?;
        let awaited: Vec<RunRow> = match &req.agents {
            Some(labels) if !labels.is_empty() => {
                let mut v = Vec::new();
                for l in labels {
                    match kids.iter().find(|k| k.label.as_deref() == Some(l.as_str())) {
                        Some(k) => v.push(k.clone()),
                        None => {
                            anyhow::bail!("no sub-agent {l:?}; yours are: {}", labels_of(&kids))
                        }
                    }
                }
                v
            }
            _ => {
                let mut v = Vec::new();
                for k in &kids {
                    if self
                        .finished_mail(&k.id)?
                        .is_none_or(|m| m.consumed_step.is_none())
                    {
                        v.push(k.clone());
                    }
                }
                v
            }
        };
        if awaited.is_empty() {
            return Ok(json!({ "agents": [], "note": "no sub-agents left to wait for" }));
        }
        let timeout =
            Duration::from_secs(req.timeout_secs.unwrap_or(DEFAULT_WAIT_SECS).clamp(1, 3600));
        let deadline = tokio::time::Instant::now() + timeout;
        let sink = progress_sink();
        let mut announced: HashSet<String> = HashSet::new();
        loop {
            let mut done: Vec<MailRow> = Vec::new();
            // Children blocked on a question: (child, question mail).
            let mut asking: Vec<(String, MailRow)> = Vec::new();
            for child in &awaited {
                match self.finished_mail(&child.id)? {
                    Some(m) => done.push(m),
                    None => {
                        if let Some(row) = self.store.run(&child.id)? {
                            if let Some((m, _)) = pending_question(&self.store, &row)? {
                                asking.push((row.id.clone(), m));
                            }
                            self.ensure_driven(&row)?;
                        }
                    }
                }
            }
            for m in &done {
                if announced.insert(m.dedupe_key.clone())
                    && let Some(s) = &sink
                    && let Ok(f) = serde_json::from_str::<Finished>(&m.body)
                {
                    s(&format!("{} {}\n", f.agent, f.status));
                }
            }
            let met = match req.mode {
                WaitMode::All => done.len() == awaited.len(),
                WaitMode::Any => !done.is_empty(),
            };
            // A child blocked on a question would deadlock an `all` wait.
            if met || !asking.is_empty() || tokio::time::Instant::now() >= deadline {
                // Reports and questions delivered here don't come back
                // through the inbox.
                let unconsumed: Vec<i64> = done
                    .iter()
                    .chain(asking.iter().map(|(_, m)| m))
                    .filter(|m| m.consumed_step.is_none())
                    .map(|m| m.id)
                    .collect();
                consume_mail(&unconsumed);
                let mut entries = Vec::new();
                for child in &awaited {
                    match done
                        .iter()
                        .find(|m| m.from_run_id.as_deref() == Some(&child.id))
                    {
                        Some(m) => entries.push(Self::report_entry(m)),
                        None => {
                            let row = self.store.run(&child.id)?.unwrap_or_else(|| child.clone());
                            entries.push(self.live_entry(&row)?);
                        }
                    }
                }
                let mut out = json!({ "agents": entries });
                // A report written by a tainted child taints its reader.
                let mut tainted = false;
                for m in &done {
                    if let Some(from) = &m.from_run_id
                        && self.store.run(from)?.is_some_and(|r| r.taint.is_some())
                    {
                        tainted = true;
                    }
                }
                if tainted {
                    out["tainted"] = true.into();
                }
                if !asking.is_empty() {
                    out["note"] = "a sub-agent asked a question and is blocked until you answer it with agent_send".into();
                } else if !met {
                    out["note"] = format!(
                        "timed out after {}s; the others are still working",
                        timeout.as_secs()
                    )
                    .into();
                }
                return Ok(out);
            }
            tokio::select! {
                r = self.sleep_or_wake(&ctx.cancel) => r?,
                () = tokio::time::sleep_until(deadline) => {}
            }
        }
    }
}

impl Subagents {
    fn try_send(&self, ctx: &ToolCtx, call_id: &str, req: SendRequest) -> anyhow::Result<Value> {
        let key = format!("send:{}:{call_id}", ctx.run_id);
        // Re-executed after a crash: it was sent already.
        if let Some(m) = self.store.mail_by_key(&key)? {
            return Ok(json!({ "agent": req.agent, "delivered_as": m.kind }));
        }
        let kids = self.store.children(&ctx.run_id)?;
        let Some(child) = kids
            .iter()
            .find(|k| k.label.as_deref() == Some(req.agent.as_str()))
        else {
            anyhow::bail!(
                "no sub-agent {:?}; yours are: {}",
                req.agent,
                labels_of(&kids)
            );
        };
        if req.message.trim().is_empty() {
            anyhow::bail!("message is empty");
        }
        if is_ended(child) {
            anyhow::bail!(
                "{} has already finished ({}); spawn a new sub-agent to continue its work",
                req.agent,
                self.status_of(child)?
            );
        }
        let pending = pending_question(&self.store, child)?;
        let (kind, question_id) = match (&pending, req.question_id) {
            (Some((m, _)), Some(q)) if q != subagent::question_id(m.id) => anyhow::bail!(
                "{} is waiting for an answer to {}, not {q}",
                req.agent,
                subagent::question_id(m.id)
            ),
            // Blocked on a question: whatever is sent is the answer.
            (Some((m, _)), _) => ("answer", Some(subagent::question_id(m.id))),
            // An answer to a question that timed out arrives as a late answer.
            (None, Some(q)) => ("answer", Some(q)),
            (None, None) => ("message", None),
        };
        let body = Note {
            text: req.message.clone(),
            question_id,
        };
        let posted = self.store.post_mail(&NewMail {
            to_run_id: &child.id,
            from_run_id: Some(&ctx.run_id),
            kind,
            body: &serde_json::to_string(&body)?,
            dedupe_key: &key,
        })?;
        if let Some((m, _)) = &pending
            && m.consumed_step.is_none()
        {
            // Answered: the question needn't reach the inbox as well.
            consume_mail(&[m.id]);
        }
        if posted {
            let _ = self.events.send(AgentEvent::SubagentMessage {
                run_id: ctx.run_id.clone(),
                to_run_id: child.id.clone(),
                kind: kind.into(),
                text: req.message,
            });
        }
        self.notify.notify_waiters();
        self.ensure_driven(child)?;
        Ok(json!({
            "agent": req.agent,
            "delivered_as": kind,
            "status": self.status_of(child)?,
        }))
    }

    async fn try_ask(
        &self,
        ctx: &ToolCtx,
        call_id: &str,
        req: AskRequest,
    ) -> anyhow::Result<Value> {
        let me = self.run(&ctx.run_id).map_err(anyhow::Error::msg)?;
        let Some(parent) = me.parent_run_id.clone() else {
            anyhow::bail!("only sub-agents can ask a parent agent");
        };
        if req.question.trim().is_empty() {
            anyhow::bail!("question is empty");
        }
        let key = format!("{ASK_PREFIX}{}:{call_id}", me.id);
        let body = Question {
            agent: me.label.clone().unwrap_or_default(),
            question: req.question.clone(),
            recommendation: req.recommendation.clone(),
        };
        // Keyed by the call id, so a re-execution doesn't ask twice.
        let posted = self.store.post_mail(&NewMail {
            to_run_id: &parent,
            from_run_id: Some(&me.id),
            kind: "question",
            body: &serde_json::to_string(&body)?,
            dedupe_key: &key,
        })?;
        let mail = self
            .store
            .mail_by_key(&key)?
            .ok_or_else(|| anyhow::anyhow!("question {key} vanished"))?;
        let qid = subagent::question_id(mail.id);
        if posted {
            tracing::info!(run = %me.id, parent = %parent, question = %qid, "sub-agent asked its parent");
            let _ = self.events.send(AgentEvent::SubagentMessage {
                run_id: me.id.clone(),
                to_run_id: parent.clone(),
                kind: "question".into(),
                text: req.question,
            });
        }
        self.notify.notify_waiters();
        // Counted from when the question was first asked, so a re-execution
        // after a crash doesn't restart the clock.
        let timeout_secs = self.config.ask_parent_timeout_secs.max(1);
        let deadline_ms = mail.created_at + (timeout_secs as i64) * 1000;
        loop {
            if let Some((answer, note)) = self.answer_for(&me.id, &qid)? {
                consume_mail(&[answer.id]);
                return Ok(json!({ "question_id": qid, "answer": note.text }));
            }
            let left = deadline_ms - now_ms();
            if left <= 0 {
                return Ok(json!({
                    "question_id": qid,
                    "answer": null,
                    "note": format!(
                        "No answer within {}. Proceed with your recommendation and say in your report what you assumed.",
                        human_secs(timeout_secs)
                    ),
                }));
            }
            tokio::select! {
                r = self.sleep_or_wake(&ctx.cancel) => r?,
                () = tokio::time::sleep(Duration::from_millis(left as u64)) => {}
            }
        }
    }

    /// The unconsumed answer to question `qid` in `run_id`'s mail.
    fn answer_for(&self, run_id: &str, qid: &str) -> anyhow::Result<Option<(MailRow, Note)>> {
        for m in self.store.pending_mail(run_id)? {
            if m.kind == "answer"
                && let Ok(n) = serde_json::from_str::<Note>(&m.body)
                && n.question_id.as_deref() == Some(qid)
            {
                return Ok(Some((m, n)));
            }
        }
        Ok(None)
    }

    /// A message from the user to a running sub-agent (`POST
    /// /v1/runs/{id}/message`), delivered at its next step.
    pub fn message_run(&self, run_id: &str, text: &str) -> anyhow::Result<()> {
        let run = self
            .store
            .run(run_id)?
            .ok_or_else(|| anyhow::anyhow!("run {run_id} not found"))?;
        if run.parent_run_id.is_none() {
            anyhow::bail!("run {run_id} is not a sub-agent; send the main agent a message instead");
        }
        if !matches!(run.status.as_str(), "running" | "queued") {
            anyhow::bail!("sub-agent run {run_id} is not running ({})", run.status);
        }
        if text.trim().is_empty() {
            anyhow::bail!("text is empty");
        }
        let body = Note {
            text: text.to_owned(),
            question_id: None,
        };
        self.store.post_mail(&NewMail {
            to_run_id: run_id,
            from_run_id: None,
            kind: "message",
            body: &serde_json::to_string(&body)?,
            dedupe_key: &format!("user:{run_id}:{}", new_id()),
        })?;
        tracing::info!(run = run_id, "user message for a sub-agent");
        self.notify.notify_waiters();
        Ok(())
    }
}

/// `900` → "15 minutes", `90` → "90 seconds".
fn human_secs(secs: u64) -> String {
    match secs {
        60 => "1 minute".into(),
        s if s % 60 == 0 => format!("{} minutes", s / 60),
        1 => "1 second".into(),
        s => format!("{s} seconds"),
    }
}

/// A child's unanswered `agent_ask_parent`: the question mail and its body.
/// Read from the store (the child's newest step is that tool call, still
/// running, and no answer has been sent for it), so it works for a child driven
/// by another process.
pub fn pending_question(
    store: &Store,
    child: &RunRow,
) -> anyhow::Result<Option<(MailRow, Question)>> {
    if child.status != "running" {
        return Ok(None);
    }
    let Some((kind, status, input)) = store.last_step(&child.id)? else {
        return Ok(None);
    };
    if kind != "tool" || status != "started" {
        return Ok(None);
    }
    let call: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let (Some("agent_ask_parent"), Some(call_id)) = (call["name"].as_str(), call["id"].as_str())
    else {
        return Ok(None);
    };
    let Some(mail) = store.mail_by_key(&format!("{ASK_PREFIX}{}:{call_id}", child.id))? else {
        return Ok(None);
    };
    let qid = subagent::question_id(mail.id);
    // Consumed answers count too: the child takes the answer in the ask
    // step's own transaction, and until that step completes it still looks
    // like the newest, started one.
    let answered = store.mail_of_kind(&child.id, "answer")?.iter().any(|m| {
        serde_json::from_str::<Note>(&m.body)
            .is_ok_and(|n| n.question_id.as_deref() == Some(qid.as_str()))
    });
    if answered {
        return Ok(None);
    }
    let q = serde_json::from_str::<Question>(&mail.body)?;
    Ok(Some((mail, q)))
}

/// `child` as front ends list it.
pub fn child_run(store: &Store, child: &RunRow) -> anyhow::Result<ChildRun> {
    let finished = store.mail_by_key(&format!("{FINISHED_PREFIX}{}", child.id))?;
    let report = finished
        .as_ref()
        .and_then(|m| serde_json::from_str::<Finished>(&m.body).ok());
    let question = pending_question(store, child)?;
    let status = match (&report, child.status.as_str()) {
        (Some(f), _) => f.status.clone(),
        (None, "running") if question.is_some() => "question".into(),
        (None, "queued" | "running") => child.status.clone(),
        (None, "interrupted") if subagent::was_cancelled(child) => "cancelled".into(),
        (None, "interrupted") => "interrupted".into(),
        (None, other) => subagent::finished_status(other, &final_text(store, child)?).into(),
    };
    let result = match &report {
        Some(f) => f.result.clone(),
        None if child.status == "completed" => Some(final_text(store, child)?),
        None => None,
    };
    let task = match child.input_message_id {
        Some(id) => store.message_text(id)?.unwrap_or_default(),
        None => String::new(),
    };
    let (input_tokens, output_tokens, cache_read_tokens, cache_write_tokens) =
        store.run_usage(&child.id)?;
    Ok(ChildRun {
        run_id: child.id.clone(),
        session_id: child.session_id.clone(),
        parent_run_id: child.parent_run_id.clone().unwrap_or_default(),
        call_id: child.parent_call_id.clone().unwrap_or_default(),
        label: child.label.clone().unwrap_or_default(),
        task: task.chars().take(subagent::TASK_PREVIEW).collect(),
        model: child.model.clone(),
        depth: child.depth,
        status,
        error: child.error.clone(),
        started_at: child.started_at / 1000,
        finished_at: child.finished_at.map(|t| t / 1000),
        usage: Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
        },
        result_preview: result
            .filter(|r| !r.trim().is_empty())
            .map(|r| r.chars().take(subagent::RESULT_PREVIEW).collect()),
        pending_question: question.map(|(_, q)| q.question),
        agent_id: delegated_agent(store, child),
    })
}

/// The profile `child` runs as when its parent delegated to one: its
/// session's agent, when that isn't its parent's.
fn delegated_agent(store: &Store, child: &RunRow) -> Option<String> {
    let parent = store.run(child.parent_run_id.as_deref()?).ok()??;
    let mine = store.session_agent(&child.session_id).ok()?;
    (mine != store.session_agent(&parent.session_id).ok()?).then_some(mine)
}

/// The children of `run_id`, oldest first; with `recursive`, their children
/// too (each after its parent).
pub fn children_of(store: &Store, run_id: &str, recursive: bool) -> anyhow::Result<Vec<ChildRun>> {
    let mut out = Vec::new();
    let mut stack: Vec<RunRow> = store.children(run_id)?.into_iter().rev().collect();
    while let Some(child) = stack.pop() {
        out.push(child_run(store, &child)?);
        if recursive && out.len() < 10_000 {
            stack.extend(store.children(&child.id)?.into_iter().rev());
        }
    }
    Ok(out)
}

fn labels_of(kids: &[RunRow]) -> String {
    let v: Vec<&str> = kids.iter().filter_map(|k| k.label.as_deref()).collect();
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}

/// `^[a-z0-9][a-z0-9-]{0,31}$`
fn valid_label(l: &str) -> bool {
    let ok = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    !l.is_empty() && l.len() <= 32 && l.starts_with(ok) && l.chars().all(|c| ok(c) || c == '-')
}

/// A run that will not make progress again on its own.
fn is_ended(run: &RunRow) -> bool {
    matches!(run.status.as_str(), "completed" | "failed") || subagent::was_cancelled(run)
}

/// The text of a run's last assistant message.
fn final_text(store: &Store, run: &RunRow) -> anyhow::Result<String> {
    let last = store
        .session_messages(&run.session_id)?
        .into_iter()
        .rev()
        .find(|m| m.run_id.as_deref() == Some(run.id.as_str()) && m.role == "assistant");
    Ok(last
        .and_then(|m| serde_json::from_str::<Vec<ContentBlock>>(&m.content_json).ok())
        .map(|blocks| {
            blocks
                .into_iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text),
                    _ => None,
                })
                .collect::<String>()
        })
        .unwrap_or_default())
}

#[async_trait]
impl SubagentHost for Subagents {
    async fn spawn(
        &self,
        ctx: &ToolCtx,
        call_id: &str,
        req: SpawnRequest,
    ) -> Result<Value, String> {
        self.try_spawn(ctx, call_id, req)
            .map_err(|e| format!("{e:#}"))
    }

    async fn wait(&self, ctx: &ToolCtx, req: WaitRequest) -> Result<Value, String> {
        self.try_wait(ctx, req).await.map_err(|e| format!("{e:#}"))
    }

    async fn result(&self, ctx: &ToolCtx, req: ResultRequest) -> Result<Value, String> {
        let kids = self.store.children(&ctx.run_id).map_err(err)?;
        let Some(child) = kids
            .iter()
            .find(|k| k.label.as_deref() == Some(req.agent.as_str()))
        else {
            return Err(format!(
                "no sub-agent {:?}; yours are: {}",
                req.agent,
                labels_of(&kids)
            ));
        };
        let row = self
            .store
            .run(&child.id)
            .map_err(err)?
            .unwrap_or_else(|| child.clone());
        if !is_ended(&row) {
            return Err(format!(
                "sub-agent {:?} is still running; use agent_wait to get its report",
                req.agent
            ));
        }
        let text = final_text(&self.store, &row).map_err(err)?;
        let max = self.limits.max_result_chars;
        let total = text.chars().count();
        let offset = req.offset.unwrap_or(0).min(total);
        let limit = req.limit.unwrap_or(max).clamp(1, max);
        let slice: String = text.chars().skip(offset).take(limit).collect();
        let end = offset + slice.chars().count();
        let mut out = json!({
            "agent": req.agent,
            "run_id": row.id,
            "total_chars": total,
            "offset": offset,
            "text": slice,
        });
        if end < total {
            out["next_offset"] = end.into();
        } else {
            out["complete"] = true.into();
        }
        if row.taint.is_some() {
            out["tainted"] = true.into();
        }
        Ok(out)
    }

    async fn cancel(&self, ctx: &ToolCtx, req: CancelRequest) -> Result<Value, String> {
        let kids = self.store.children(&ctx.run_id).map_err(err)?;
        let Some(child) = kids
            .iter()
            .find(|k| k.label.as_deref() == Some(req.agent.as_str()))
        else {
            return Err(format!(
                "no sub-agent {:?}; yours are: {}",
                req.agent,
                labels_of(&kids)
            ));
        };
        if is_ended(child) {
            let status = self.status_of(child).map_err(err)?;
            return Ok(json!({ "agent": req.agent, "status": status }));
        }
        let reason = match req.reason.as_deref().map(str::trim) {
            Some(r) if !r.is_empty() => format!("cancelled by the parent agent: {r}"),
            _ => "cancelled by the parent agent".to_owned(),
        };
        self.cancel_child(child, &reason).map_err(err)?;
        Ok(json!({ "agent": req.agent, "status": "cancelled" }))
    }

    async fn status(&self, ctx: &ToolCtx) -> Result<Value, String> {
        let kids = self.store.children(&ctx.run_id).map_err(err)?;
        let mut out = Vec::new();
        for k in &kids {
            let mut v = self.live_entry(k).map_err(err)?;
            let (i, o, cr, cw) = self.store.run_usage(&k.id).map_err(err)?;
            v["tokens"] = (i + o + cr + cw).into();
            let mail = self.finished_mail(&k.id).map_err(err)?;
            v["reported"] = mail.is_some_and(|m| m.consumed_step.is_some()).into();
            out.push(v);
        }
        Ok(json!({ "agents": out }))
    }

    async fn send(&self, ctx: &ToolCtx, call_id: &str, req: SendRequest) -> Result<Value, String> {
        self.try_send(ctx, call_id, req)
            .map_err(|e| format!("{e:#}"))
    }

    async fn ask_parent(
        &self,
        ctx: &ToolCtx,
        call_id: &str,
        req: AskRequest,
    ) -> Result<Value, String> {
        self.try_ask(ctx, call_id, req)
            .await
            .map_err(|e| format!("{e:#}"))
    }

    fn inbox_due(&self, run_id: &str, or_live: bool) -> Result<bool, StepError> {
        if !self.store.pending_mail(run_id)?.is_empty() {
            return Ok(true);
        }
        if !or_live {
            return Ok(false);
        }
        for child in self.store.children(run_id)? {
            if self
                .store
                .mail_by_key(&format!("{FINISHED_PREFIX}{}", child.id))?
                .is_none()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn take_mail(
        &self,
        run_id: &str,
        wait: bool,
        cancel: &CancellationToken,
    ) -> Result<Vec<MailRow>, StepError> {
        loop {
            let mail = self.store.pending_mail(run_id)?;
            if !mail.is_empty() || !wait {
                return Ok(mail);
            }
            let mut outstanding = false;
            for child in self.store.children(run_id)? {
                if self
                    .store
                    .mail_by_key(&format!("{FINISHED_PREFIX}{}", child.id))?
                    .is_none()
                {
                    outstanding = true;
                    self.ensure_driven(&child)
                        .map_err(|e| StepError::Failed(format!("{e:#}")))?;
                }
            }
            if !outstanding {
                return Ok(Vec::new());
            }
            self.sleep_or_wake(cancel).await?;
        }
    }

    fn interrupt_reason(&self, run: &RunRow) -> Option<String> {
        let recorded = self.reasons.lock().ok().and_then(|mut r| r.remove(&run.id));
        match recorded {
            Some(r) => Some(r),
            None if run.parent_run_id.is_some() => Some(USER_CANCELLED.into()),
            None => None,
        }
    }

    async fn run_ended(&self, run: &RunRow, outcome: &RunOutcome) {
        // Children can't outlive their parent's run.
        match self.store.children(&run.id) {
            Ok(kids) => {
                for k in kids.iter().filter(|k| !is_ended(k)) {
                    if let Err(e) = self.cancel_child(k, PARENT_ENDED) {
                        tracing::warn!(run = %k.id, error = %e, "cancelling a sub-agent failed");
                    }
                }
            }
            Err(e) => tracing::warn!(run = %run.id, error = %e, "listing sub-agents failed"),
        }
        if run.parent_run_id.is_some() {
            if let Err(e) = self.report(run, Some(outcome)) {
                tracing::error!(run = %run.id, error = %e, "sub-agent report failed");
            }
            self.kick();
        }
    }
}

/// A child's step and token limits, and which asks the ceilings lowered.
#[derive(Debug, PartialEq, Eq)]
struct ChildLimits {
    max_iterations: u32,
    budget_tokens: Option<u64>,
    lowered: Vec<String>,
}

/// The configured limits are defaults; `agent_spawn` may ask for more, up
/// to the ceilings, so a retry after running out can actually get more.
fn child_limits(limits: &SubagentLimits, req: &SpawnRequest) -> ChildLimits {
    let mut lowered = Vec::new();
    let max_iterations = match req.max_iterations {
        None => limits.max_iterations,
        Some(n) if n > limits.max_iterations_ceiling => {
            lowered.push(format!(
                "max_iterations {n} → {}",
                limits.max_iterations_ceiling
            ));
            limits.max_iterations_ceiling
        }
        Some(n) => n.max(1),
    };
    // 0 in config = no budget / no ceiling.
    let budget_tokens = match req.budget_tokens {
        None => (limits.budget_tokens > 0).then_some(limits.budget_tokens),
        Some(n) if limits.budget_tokens_ceiling > 0 && n > limits.budget_tokens_ceiling => {
            lowered.push(format!(
                "budget_tokens {n} → {}",
                limits.budget_tokens_ceiling
            ));
            Some(limits.budget_tokens_ceiling)
        }
        Some(n) => Some(n),
    };
    ChildLimits {
        max_iterations,
        budget_tokens,
        lowered,
    }
}

/// Appended to a child's system prompt so it can pace itself.
fn limits_section(max_iterations: u32, budget: Option<u64>) -> String {
    let tokens = match budget {
        Some(b) => format!(
            " and {b} tokens (every model call counts its whole input again, so long files and \
             outputs use it up fast)"
        ),
        None => String::new(),
    };
    format!(
        "## Your limits\n\nYou have {max_iterations} steps{tokens}. Read and search only what the task \
         needs. When you reach a limit you get one last turn, without tools, to write your report."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_limits_default_and_ceiling() {
        let limits = SubagentLimits::from(&SubagentsConfig::default());
        let req = |iters: Option<u32>, budget: Option<u64>| SpawnRequest {
            label: "a".into(),
            task: "t".into(),
            tools: None,
            model: None,
            max_iterations: iters,
            budget_tokens: budget,
            post: false,
            agent: None,
            subdir: None,
        };
        let got = child_limits(&limits, &req(None, None));
        assert_eq!((got.max_iterations, got.budget_tokens), (30, Some(400_000)));
        // Asking for more than the default works: a retry can grow.
        let got = child_limits(&limits, &req(Some(60), Some(1_000_000)));
        assert_eq!(
            (got.max_iterations, got.budget_tokens),
            (60, Some(1_000_000))
        );
        assert!(got.lowered.is_empty());
        // Past the ceiling it is lowered, and says so.
        let got = child_limits(&limits, &req(Some(500), Some(9_000_000)));
        assert_eq!(
            (got.max_iterations, got.budget_tokens),
            (100, Some(2_000_000))
        );
        assert_eq!(got.lowered.len(), 2);
        // 0 = no budget and no ceiling.
        let open = SubagentLimits {
            budget_tokens: 0,
            budget_tokens_ceiling: 0,
            ..limits
        };
        assert_eq!(child_limits(&open, &req(None, None)).budget_tokens, None);
        assert_eq!(
            child_limits(&open, &req(None, Some(9_000_000))).budget_tokens,
            Some(9_000_000)
        );
    }

    /// A host over an in-memory store with a parent run `p` and two ended
    /// children whose reports are `chars` long: `clean` and `dirty` (tainted).
    fn host_with_children(chars: usize) -> (Subagents, ToolCtx) {
        let store = Store::open_in_memory().unwrap();
        let journal = Journal::new(store.clone());
        let config = tern_core::Config::default();
        let host = Subagents::new(
            store.clone(),
            journal.clone(),
            broadcast::channel(8).0,
            Weak::new(),
            &config,
            Arc::new(RwLock::new(config.clone())),
            true,
        );
        let input = journal.to_row(&Message::user_text("brief")).unwrap();
        let mk = |id: &str, parent: Option<&str>| {
            store
                .create_session(&format!("s-{id}"), "subagent", None)
                .unwrap();
            store
                .create_run(
                    &NewRun {
                        id,
                        session_id: &format!("s-{id}"),
                        trigger: "subagent",
                        model: "mock:mock",
                        system_prompt: "system",
                        parent_run_id: parent,
                        parent_call_id: parent.map(|_| id),
                        label: parent.map(|_| id),
                        depth: u32::from(parent.is_some()),
                        ..NewRun::default()
                    },
                    &input,
                )
                .unwrap();
        };
        mk("p", None);
        for id in ["clean", "dirty"] {
            mk(id, Some("p"));
            let report = journal
                .to_row(&Message {
                    role: tern_core::types::Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "é".repeat(chars),
                    }],
                })
                .unwrap();
            store.start_step(id, 0, "llm", "h", "{}").unwrap();
            store.complete_step(id, 0, "{}", None, &[report]).unwrap();
            store.set_run_status(id, "completed", None).unwrap();
            if id == "dirty" {
                store.set_run_taint(id, "result of http_fetch").unwrap();
            }
            let row = store.run(id).unwrap().unwrap();
            host.report(&row, None).unwrap();
        }
        let ctx = ToolCtx {
            scope: tern_core::workspace::WorkspaceScope::for_dir(std::path::PathBuf::new()),
            run_id: "p".into(),
            session_id: "s-p".into(),
            cancel: CancellationToken::new(),
        };
        (host, ctx)
    }

    #[tokio::test]
    async fn a_long_report_reaches_the_parent_cut_and_the_rest_is_fetchable() {
        let (host, ctx) = host_with_children(20_000);
        let wait = |agents: &[&str]| WaitRequest {
            agents: Some(agents.iter().map(|a| (*a).to_owned()).collect()),
            ..WaitRequest::default()
        };
        let out = host.wait(&ctx, wait(&["clean"])).await.unwrap();
        let entry = &out["agents"][0];
        let result = entry["result"].as_str().unwrap();
        assert_eq!(
            result.chars().filter(|c| *c == 'é').count(),
            subagent::DEFAULT_RESULT_CHARS
        );
        assert!(
            result.contains("report truncated: showing the first 8000 of 20000 characters")
                && result.contains("agent_result {\"agent\": \"clean\", \"offset\": 8000}"),
            "{result}"
        );
        assert!(out.get("tainted").is_none(), "a clean child is not framed");

        // Reading on: slices of at most the cap, until complete.
        let mut offset = 8_000;
        let mut got = 0;
        loop {
            let r = host
                .result(
                    &ctx,
                    ResultRequest {
                        agent: "clean".into(),
                        offset: Some(offset),
                        limit: None,
                    },
                )
                .await
                .unwrap();
            assert_eq!(r["total_chars"], 20_000);
            got += r["text"].as_str().unwrap().chars().count();
            match r["next_offset"].as_u64() {
                Some(n) => offset = n as usize,
                None => {
                    assert_eq!(r["complete"], true);
                    break;
                }
            }
        }
        assert_eq!(got, 12_000);
        // An unknown label and a running child are errors the model can read.
        let e = host
            .result(
                &ctx,
                ResultRequest {
                    agent: "nope".into(),
                    offset: None,
                    limit: None,
                },
            )
            .await
            .unwrap_err();
        assert!(e.contains("no sub-agent"), "{e}");
    }

    #[tokio::test]
    async fn a_tainted_childs_report_taints_whoever_reads_it() {
        let (host, ctx) = host_with_children(100);
        let waited = host
            .wait(
                &ctx,
                WaitRequest {
                    agents: Some(vec!["dirty".into()]),
                    ..WaitRequest::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(waited["tainted"], true, "{waited}");
        let fetched = host
            .result(
                &ctx,
                ResultRequest {
                    agent: "dirty".into(),
                    offset: None,
                    limit: Some(10),
                },
            )
            .await
            .unwrap();
        assert_eq!(fetched["tainted"], true, "{fetched}");
    }

    #[test]
    fn labels() {
        for ok in ["a", "pricing-research", "r1", &"a".repeat(32)] {
            assert!(valid_label(ok), "{ok}");
        }
        for bad in ["", "-a", "A", "a_b", "a b", &"a".repeat(33)] {
            assert!(!valid_label(bad), "{bad}");
        }
    }
}
