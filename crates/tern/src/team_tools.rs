//! Tools for agents that work as a team: `brief_team` (tell teammates about
//! a decision) and `inspect_work` (look at your own runs). Both only read
//! and write tern's own store; the host decides who may do what.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult, current_call_id};
use tern_core::types::{ToolCall, ToolSpec};
use tern_store::{AgentRow, NewPost, RunRow, Store};

use tern_tools::channels::OnPost;

fn spec(name: &str, description: &str, input_schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
    }
}

fn args<T: serde::de::DeserializeOwned>(raw: Value) -> Result<T, ToolResult> {
    serde_json::from_value(raw).map_err(|e| ToolResult::error(format!("invalid arguments: {e}")))
}

/// Post kinds a brief is written as. A plain brief is read at the
/// teammate's next run; the other two also wake it.
pub const BRIEF: &str = "brief";
pub const BRIEF_NOW: &str = "brief_now";
pub const BRIEF_REPLY: &str = "brief_reply";

/// The agent `name` (id, slug or name) among `agents`.
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn find<'a>(agents: &'a [AgentRow], name: &str) -> Option<&'a AgentRow> {
    let wanted = name.trim().trim_start_matches('@');
    agents
        .iter()
        .find(|a| a.id == wanted || a.slug == wanted || a.name.eq_ignore_ascii_case(wanted))
}

fn roster(agents: &[AgentRow], except: &str) -> String {
    let v: Vec<&str> = agents
        .iter()
        .filter(|a| a.id != except)
        .map(|a| a.slug.as_str())
        .collect();
    if v.is_empty() {
        "none".into()
    } else {
        v.join(", ")
    }
}

// ---- brief_team ----

pub struct BriefTeam {
    store: Store,
    on_post: Option<OnPost>,
}

impl BriefTeam {
    pub fn new(store: Store, on_post: Option<OnPost>) -> Self {
        Self { store, on_post }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BriefArgs {
    agents: Vec<String>,
    message: String,
    #[serde(default)]
    now: bool,
    #[serde(default)]
    want_reply: bool,
}

#[async_trait]
impl Tool for BriefTeam {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "brief_team",
            "Tell teammates about a decision or a change of plan, so their work matches. Use it whenever the user changes a plan, a priority or a fact others rely on. By default each teammate reads the brief at its next run, without being woken. `now` wakes them to act on it at once; `want_reply` wakes them and asks for an answer back. Say what changed and what it means for their work, in a few lines.",
            json!({
                "type": "object",
                "required": ["agents", "message"],
                "properties": {
                    "agents": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 8,
                        "description": "Teammates' names, e.g. ['scout', 'researcher']." },
                    "message": { "type": "string" },
                    "now": { "type": "boolean", "description": "Wake them to act on it now (default false)." },
                    "want_reply": { "type": "boolean", "description": "Wake them and ask for a reply (default false)." }
                }
            }),
        )
    }

    /// Granted: it only writes tern's own store; a tainted run's brief
    /// carries the taint on, to whoever reads it.
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::granted_write(true)
    }

    fn summary(&self, args: &Value) -> Option<String> {
        let names: Vec<String> = args
            .get("agents")?
            .as_array()?
            .iter()
            .filter_map(|a| a.as_str().map(str::to_owned))
            .collect();
        Some(format!(
            "Briefed {}",
            tern_tools::agents::names_list(&names)
        ))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: BriefArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        match self.brief(ctx, a) {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => ToolResult::error(e),
        }
    }
}

impl BriefTeam {
    fn brief(&self, ctx: &ToolCtx, a: BriefArgs) -> Result<Value, String> {
        let store = &self.store;
        let message = a.message.trim();
        if message.is_empty() {
            return Err("message is empty: say what changed".into());
        }
        let run: RunRow = store
            .run(&ctx.run_id)
            .map_err(err)?
            .ok_or("run not found")?;
        let me_id = store.session_agent(&ctx.session_id).map_err(err)?;
        let me = store
            .agent(&me_id)
            .map_err(err)?
            .ok_or("you aren't an agent")?;
        let agents = store.agents(false).map_err(err)?;
        // Every name is checked before anything is written.
        let mut targets: Vec<&AgentRow> = Vec::new();
        for name in &a.agents {
            let Some(t) = find(&agents, name) else {
                return Err(format!(
                    "no teammate {name:?}; your teammates are: {}",
                    roster(&agents, &me.id)
                ));
            };
            if t.id == me.id {
                return Err("that's you: brief your teammates, not yourself".into());
            }
            if !targets.iter().any(|x| x.id == t.id) {
                targets.push(t);
            }
        }
        let wake = a.now || a.want_reply;
        if wake && let Some(t) = targets.iter().find(|t| !me.may_delegate_to(t)) {
            return Err(format!(
                "you can brief {} for their next run, but you can't wake them: they aren't in your delegates",
                t.name
            ));
        }
        let kind = match (a.want_reply, a.now) {
            (true, _) => BRIEF_REPLY,
            (false, true) => BRIEF_NOW,
            _ => BRIEF,
        };
        let call = current_call_id().unwrap_or_else(tern_core::agent::new_id);
        let mut told = Vec::new();
        for t in targets {
            // In the teammate's own conversation: its next run is shown it
            // with the other new posts there.
            let Some(dm) = store.agent_channel(&t.id).map_err(err)? else {
                return Err(format!("{} has no conversation to brief", t.name));
            };
            let key = format!("brief:{}:{call}:{}", run.id, t.id);
            let (row, created) = store
                .add_post(&NewPost {
                    channel_id: &dm.id,
                    session_id: Some(&ctx.session_id),
                    run_id: Some(&run.id),
                    author_kind: "agent",
                    author_id: &me.id,
                    author: &me.name,
                    kind,
                    text: message,
                    tainted: run.taint.is_some(),
                    dedupe_key: Some(&key),
                    ..NewPost::default()
                })
                .map_err(err)?;
            if created && let Some(f) = &self.on_post {
                f(row);
            }
            told.push(json!({ "agent": t.slug, "name": t.name, "wakes": wake }));
        }
        Ok(json!({
            "briefed": told,
            "note": if wake {
                "they were woken to act on it"
            } else {
                "they will read it at their next run"
            },
        }))
    }
}

// ---- inspect_work ----

pub struct InspectWork {
    store: Store,
}

impl InspectWork {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectArgs {
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

const DEFAULT_RUNS: usize = 5;
const MAX_RUNS: usize = 20;
const MAX_STEPS: usize = 80;

#[async_trait]
impl Tool for InspectWork {
    fn effect(&self) -> Effect {
        Effect::Read
    }

    fn spec(&self) -> ToolSpec {
        spec(
            "inspect_work",
            "Look at your own recent runs instead of guessing what happened: for each, its status, how long it took, the steps it took with timings and failures, the hand-offs it made, the models it used and the tokens it spent. Without `run_id`, lists your latest runs; with one, shows that run step by step. Only your own work.",
            json!({
                "type": "object",
                "properties": {
                    "run_id": { "type": "string", "description": "A run of yours (or a sub-agent it started) to show step by step." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_RUNS, "description": "How many runs to list (default 5)." }
                }
            }),
        )
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: InspectArgs = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        match self.inspect(ctx, a) {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => ToolResult::error(e),
        }
    }
}

fn secs(ms: i64) -> f64 {
    (ms as f64 / 100.0).round() / 10.0
}

impl InspectWork {
    fn inspect(&self, ctx: &ToolCtx, a: InspectArgs) -> Result<Value, String> {
        let store = &self.store;
        let me = store.session_agent(&ctx.session_id).map_err(err)?;
        if let Some(id) = a.run_id.as_deref() {
            let run = store
                .run(id)
                .map_err(err)?
                .ok_or_else(|| format!("no run {id:?}"))?;
            if !self.is_mine(&run, &me)? {
                return Err("that run isn't yours: you can look at your own work only".into());
            }
            return self.detail(&run);
        }
        let limit = a.limit.unwrap_or(DEFAULT_RUNS).clamp(1, MAX_RUNS);
        let mut runs = Vec::new();
        // Newest first; scan further back than `limit` since other agents'
        // runs are mixed in.
        for r in store.recent_runs(limit * 40).map_err(err)? {
            if runs.len() >= limit {
                break;
            }
            if r.parent_run_id.is_none() && self.is_mine(&r, &me)? {
                runs.push(self.summary(&r)?);
            }
        }
        Ok(json!({ "runs": runs }))
    }

    /// A top-level run of this agent's, or a sub-agent below one.
    fn is_mine(&self, run: &RunRow, me: &str) -> Result<bool, String> {
        let store = &self.store;
        let mut top = run.clone();
        while let Some(parent) = top.parent_run_id.clone() {
            top = store
                .run(&parent)
                .map_err(err)?
                .ok_or("a run's parent is missing")?;
        }
        Ok(store.session_agent(&top.session_id).map_err(err)? == me)
    }

    fn usage(&self, run: &RunRow) -> Result<Value, String> {
        let (i, o, cr, cw) = self.store.run_usage(&run.id).map_err(|e| e.to_string())?;
        Ok(
            json!({ "input": i, "output": o, "cache_read": cr, "cache_write": cw, "total": i + o + cr + cw }),
        )
    }

    fn summary(&self, run: &RunRow) -> Result<Value, String> {
        let steps = self.store.steps(&run.id).map_err(err)?;
        let failed = steps.iter().filter(|s| s.status == "failed").count();
        let kids = self.store.children(&run.id).map_err(err)?;
        let mut v = json!({
            "run_id": run.id,
            "status": run.status,
            "trigger": run.trigger,
            "model": run.model,
            "started_at": run.started_at / 1000,
            "steps": steps.len(),
            "failed_steps": failed,
            "hand_offs": kids.len(),
            "tokens": self.usage(run)?,
        });
        if let Some(f) = run.finished_at {
            v["seconds"] = secs(f - run.started_at).into();
        }
        if let Some(e) = &run.error {
            v["error"] = e.clone().into();
        }
        Ok(v)
    }

    fn detail(&self, run: &RunRow) -> Result<Value, String> {
        let store = &self.store;
        let steps = store.steps(&run.id).map_err(err)?;
        let mut models: Vec<String> = run.model.iter().cloned().collect();
        let mut lines = Vec::new();
        for s in steps.iter().take(MAX_STEPS) {
            let mut line = json!({ "step": s.idx, "kind": s.kind, "status": s.status });
            if let Some(f) = s.finished_at {
                line["seconds"] = secs(f - s.started_at).into();
            }
            if matches!(s.kind.as_str(), "tool" | "approval")
                && let Ok(call) = serde_json::from_str::<ToolCall>(&s.input_json)
            {
                line["tool"] = call.name.into();
            }
            if let Some(e) = &s.error {
                line["error"] = e.chars().take(300).collect::<String>().into();
            }
            let answered_by = s
                .usage_json
                .as_deref()
                .and_then(|u| serde_json::from_str::<Value>(u).ok())
                .and_then(|u| u.get("model").and_then(Value::as_str).map(str::to_owned));
            if let Some(m) = answered_by {
                line["answered_by"] = m.clone().into();
                if !models.contains(&m) {
                    models.push(m);
                }
            }
            lines.push(line);
        }
        let mut hand_offs = Vec::new();
        for k in store.children(&run.id).map_err(err)? {
            let mut h = json!({
                "label": k.label,
                "run_id": k.id,
                "status": k.status,
                "model": k.model,
                "tokens": self.usage(&k)?,
            });
            if let Some(f) = k.finished_at {
                h["seconds"] = secs(f - k.started_at).into();
            }
            if let Some(e) = &k.error {
                h["error"] = e.chars().take(300).collect::<String>().into();
            }
            hand_offs.push(h);
        }
        let mut v = self.summary(run)?;
        v["models"] = json!(models);
        v["step_list"] = json!(lines);
        if steps.len() > MAX_STEPS {
            v["steps_left_out"] = (steps.len() - MAX_STEPS).into();
        }
        v["hand_off_list"] = json!(hand_offs);
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_by_slug_name_or_id() {
        let a = |id: &str, slug: &str, name: &str| AgentRow {
            id: id.into(),
            slug: slug.into(),
            name: name.into(),
            ..AgentRow::default()
        };
        let agents = vec![a("tern", "tern", "Tern"), a("tern:scout", "scout", "Scout")];
        assert_eq!(
            find(&agents, "@Scout").map(|a| a.id.as_str()),
            Some("tern:scout")
        );
        assert_eq!(
            find(&agents, "tern:scout").map(|a| a.slug.as_str()),
            Some("scout")
        );
        assert!(find(&agents, "nobody").is_none());
        assert_eq!(roster(&agents, "tern"), "scout");
    }
}
