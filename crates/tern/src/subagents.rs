//! The sub-agent host (docs/subagents-design.md §1.3–§1.6): creates child
//! runs for `agent_spawn`, hands them to a [`RunDriver`], and delivers their
//! results to the parent through `finished` mail.
//!
//! Lives with `Runtime` rather than the daemon, so sub-agents work wherever
//! runs are driven: the daemon drives children through its `Manager`, the
//! embedded TUI and `tern run` through [`LocalDriver`].

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tern_core::agent::{Agent, RunOutcome, RunStatus, new_id};
use tern_core::config::{ModelRef, SubagentsConfig};
use tern_core::events::AgentEvent;
use tern_core::journal::{Journal, StepError};
use tern_core::subagent::{
    self, AGENT_TOOLS, CHILD_EXCLUDED_TOOLS, CancelRequest, FINISHED_PREFIX, Finished,
    READ_ONLY_TOOLS, SpawnRequest, SubagentHost, SubagentLimits, ToolsArg, WaitMode, WaitRequest,
};
use tern_core::tool::{ToolCtx, ToolRegistry, consume_mail, progress_sink};
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
    /// Configured provider names; any is accepted with the mock provider.
    providers: BTreeSet<String>,
    mock: bool,
    driver: RwLock<Option<Arc<dyn RunDriver>>>,
    /// Woken when mail is posted.
    notify: Notify,
    /// Why runs were cancelled, until their drive ends and records it.
    reasons: Mutex<HashMap<String, String>>,
    /// Serializes "is there room?" decisions (spawning and dequeueing).
    gate: Mutex<()>,
}

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
        mock: bool,
    ) -> Self {
        Self {
            store,
            journal,
            events,
            tools,
            limits: SubagentLimits::from(&config.subagents),
            config: config.subagents.clone(),
            providers: config.providers.keys().cloned().collect(),
            mock,
            driver: RwLock::new(None),
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
        let body = Finished {
            agent: row.label.clone().unwrap_or_default(),
            run_id: row.id.clone(),
            status: status.into(),
            result: (!text.trim().is_empty()).then(|| subagent::cap(&text, subagent::RESULT_MAX)),
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

    fn pick_model(&self, requested: Option<&str>, parent: &RunRow) -> Result<String, String> {
        let name = requested
            .map(|m| {
                self.config
                    .models
                    .get(m)
                    .cloned()
                    .unwrap_or_else(|| m.to_owned())
            })
            .or_else(|| self.config.default_model.clone())
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
        if !self.mock && !self.providers.contains(&m.provider) {
            return Err(format!(
                "provider {:?} is not configured; use an alias ({}) or a configured provider",
                m.provider,
                aliases()
            ));
        }
        Ok(m.to_string())
    }

    /// The tool names a child at `depth` may use.
    fn scope_for(&self, tools: Option<&ToolsArg>, depth: u32) -> Result<Vec<String>, String> {
        let registry = self.tools.upgrade().ok_or("tools are gone")?;
        let all = registry.names();
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
            !CHILD_EXCLUDED_TOOLS.contains(&n.as_str()) && !AGENT_TOOLS.contains(&n.as_str())
        });
        if depth < self.limits.max_depth {
            names.extend(
                AGENT_TOOLS
                    .iter()
                    .map(|t| (*t).to_owned())
                    .filter(|t| all.contains(t)),
            );
        }
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
        if child.status == "running"
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
        let model = self
            .pick_model(req.model.as_deref(), &parent)
            .map_err(anyhow::Error::msg)?;
        let scope = self
            .scope_for(req.tools.as_ref(), depth)
            .map_err(anyhow::Error::msg)?;
        let max_iterations = req.max_iterations.map_or(self.limits.max_iterations, |n| {
            n.clamp(1, self.limits.max_iterations)
        });
        // 0 in config = no limit.
        let budget = match (self.limits.budget_tokens, req.budget_tokens) {
            (0, asked) => asked,
            (limit, Some(asked)) => Some(asked.min(limit)),
            (limit, None) => Some(limit),
        };

        let session_id = new_id();
        let run_id = new_id();
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
                    // The parent's frozen prompt; a sub-agent role section
                    // comes with the prompt work (S5).
                    system_prompt: parent.system_prompt.as_deref().unwrap_or_default(),
                    parent_run_id: Some(&parent.id),
                    parent_call_id: Some(call_id),
                    label: Some(&req.label),
                    depth,
                    tool_scope: Some(&serde_json::to_string(&scope)?),
                    budget_tokens: budget,
                    max_iterations: Some(max_iterations),
                    queued,
                },
                &input,
            )?;
            queued
        };
        let status = if queued { "queued" } else { "running" };
        tracing::info!(run = %run_id, parent = %parent.id, label = %req.label, status, "sub-agent spawned");
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
        });
        if !queued {
            self.start(&run_id, false);
        }
        Ok(json!({ "agent": req.label, "run_id": run_id, "status": status, "model": model }))
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
            for child in &awaited {
                match self.finished_mail(&child.id)? {
                    Some(m) => done.push(m),
                    None => {
                        if let Some(row) = self.store.run(&child.id)? {
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
            if met || tokio::time::Instant::now() >= deadline {
                let unconsumed: Vec<i64> = done
                    .iter()
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
                if !met {
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

#[cfg(test)]
mod tests {
    use super::*;

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
