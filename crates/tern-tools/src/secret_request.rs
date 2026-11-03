//! `secret_request`: the agent asks the user to enter secrets in a private
//! prompt (docs/secret-requests-spec.md). The model only learns whether each
//! one was set, never the value.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tern_core::Secrets;
use tern_core::approval::{ApprovalHandler, SecretAsk, SecretRequest as Request};
use tern_core::secrets::keys::normalize_key;
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult, current_call_id};
use tern_core::types::ToolSpec;
use tern_store::Store;

use crate::{args, spec};

pub const NAME: &str = "secret_request";
const MAX_NAMES: usize = 5;
const MAX_REASON: usize = 300;
/// Prompts one run may open, so a confused agent can't keep asking.
const MAX_PROMPTS_PER_RUN: u32 = 5;
/// `secret_request_timeout_secs = 0` would end every prompt at once.
const MIN_TIMEOUT: Duration = Duration::from_secs(1);

/// `(secret, what reads it)` pairs, computed when a request is made so they
/// include connector drafts written moments ago.
pub type SecretUses = Arc<dyn Fn() -> Vec<(String, String)> + Send + Sync>;

#[derive(Default)]
struct RunState {
    prompts: u32,
    skipped: HashSet<String>,
}

pub struct SecretRequest {
    asker: Arc<dyn ApprovalHandler>,
    secrets: Secrets,
    store: Store,
    uses: SecretUses,
    timeout: Duration,
    runs: Mutex<HashMap<String, RunState>>,
}

impl SecretRequest {
    pub fn new(
        asker: Arc<dyn ApprovalHandler>,
        secrets: Secrets,
        store: Store,
        uses: SecretUses,
        timeout: Duration,
    ) -> Self {
        Self {
            asker,
            secrets,
            store,
            uses,
            timeout: timeout.max(MIN_TIMEOUT),
            runs: Mutex::default(),
        }
    }

    fn with_run<T>(&self, run_id: &str, f: impl FnOnce(&mut RunState) -> T) -> T {
        let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        if runs.len() > 1000 && !runs.contains_key(run_id) {
            runs.clear();
        }
        f(runs.entry(run_id.to_owned()).or_default())
    }
}

#[derive(Deserialize)]
struct Args {
    names: Vec<String>,
    reason: String,
    #[serde(default)]
    replace: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Set,
    AlreadySet,
    Skipped,
    TimedOut,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::AlreadySet => "already_set",
            Self::Skipped => "skipped",
            Self::TimedOut => "timed_out",
        }
    }
}

fn check(a: &Args) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = Vec::new();
    for n in &a.names {
        let n = n.trim();
        tern_core::secrets::check_name(n).map_err(|e| e.to_string())?;
        if !names.iter().any(|x| x == n) {
            names.push(n.to_owned());
        }
    }
    if names.is_empty() {
        return Err("names is empty".into());
    }
    if names.len() > MAX_NAMES {
        return Err(format!("ask for at most {MAX_NAMES} secrets at a time"));
    }
    if a.reason.trim().is_empty() {
        return Err("reason is empty: say what the secret is for".into());
    }
    if a.reason.chars().count() > MAX_REASON {
        return Err(format!("reason is longer than {MAX_REASON} characters"));
    }
    Ok(names)
}

fn result(outcomes: &BTreeMap<String, Outcome>, note: Option<&str>) -> ToolResult {
    let map: serde_json::Map<String, Value> = outcomes
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.as_str().into())))
        .collect();
    let mut text = Value::Object(map).to_string();
    if outcomes.values().any(|o| *o == Outcome::Skipped) {
        text.push('\n');
        text.push_str(
            note.unwrap_or("The user chose not to provide it. Don't ask for it in the chat."),
        );
    } else if outcomes.values().any(|o| *o == Outcome::TimedOut) {
        text.push_str("\nNobody answered the prompt in time. Don't ask for it in the chat.");
    }
    ToolResult::ok(text)
}

#[async_trait]
impl Tool for SecretRequest {
    fn effect(&self) -> Effect {
        Effect::Write
    }

    fn spec(&self) -> ToolSpec {
        spec(
            NAME,
            "Ask the user to enter secrets (API keys, tokens) in a private prompt. You never see the values: the result says only whether each was set. Use this whenever a connector, MCP server or provider needs a secret that isn't set; never ask for secrets in the chat. Secrets that are already set aren't asked for again unless `replace` is true (for example after the service rejected the key).",
            json!({
                "type": "object",
                "properties": {
                    "names": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Secret names, e.g. WEATHER_API_KEY (letters, digits and _; at most 5)"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Shown to the user: what the secret is for and what will use it (at most 300 characters)"
                    },
                    "replace": {
                        "type": "boolean",
                        "description": "Ask to replace values that are already set"
                    }
                },
                "required": ["names", "reason"]
            }),
        )
    }

    /// The prompt is itself the user's decision, and asking again after a
    /// crash only asks for what is still missing.
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        // The user is the gate: they enter the values or skip.
        ToolPolicy::granted_write(true)
    }

    fn summary(&self, raw: &Value) -> Option<String> {
        let names: Vec<&str> = raw
            .get("names")?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        Some(format!("secret_request {}", names.join(", ")))
    }

    async fn call(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let a: Args = match args(raw) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let names = match check(&a) {
            Ok(n) => n,
            Err(e) => return ToolResult::error(e),
        };

        // Loads every connector file: keep it off the async workers.
        let uses = {
            let f = self.uses.clone();
            tokio::task::spawn_blocking(move || f())
                .await
                .unwrap_or_default()
        };
        let statuses = self.secrets.statuses(&uses);
        let mut outcomes = BTreeMap::new();
        let mut asks = Vec::new();
        for name in names {
            let st = statuses.iter().find(|s| s.name == name);
            let set = st.is_some_and(|s| s.is_set());
            if set && !a.replace {
                outcomes.insert(name, Outcome::AlreadySet);
            } else if self.with_run(&ctx.run_id, |r| r.skipped.contains(&name)) {
                outcomes.insert(name, Outcome::Skipped);
            } else {
                asks.push(SecretAsk {
                    in_file: st.is_some_and(|s| s.in_file),
                    in_env: st.is_some_and(|s| s.in_env),
                    used_by: uses
                        .iter()
                        .filter(|(n, _)| *n == name)
                        .map(|(_, u)| u.clone())
                        .collect(),
                    name,
                });
            }
        }
        if asks.is_empty() {
            return result(&outcomes, None);
        }
        let over = self.with_run(&ctx.run_id, |r| {
            r.prompts += 1;
            r.prompts > MAX_PROMPTS_PER_RUN
        });
        if over {
            return ToolResult::error(format!(
                "this run has already asked for secrets {MAX_PROMPTS_PER_RUN} times; \
                 tell the user what is missing instead"
            ));
        }

        let run = self.store.run(&ctx.run_id).ok().flatten();
        let req = Request {
            id: tern_core::agent::new_id(),
            run_id: ctx.run_id.clone(),
            session_id: ctx.session_id.clone(),
            call_id: current_call_id(),
            asks,
            reason: a.reason.trim().to_owned(),
            replace: a.replace,
            parent_run_id: run.as_ref().and_then(|r| r.parent_run_id.clone()),
            agent_label: run.and_then(|r| r.label),
        };
        let reply = tokio::select! {
            r = tokio::time::timeout(self.timeout, self.asker.ask_secrets(&req)) => r,
            () = ctx.cancel.cancelled() => return ToolResult::error("cancelled"),
        };
        let mut note = None;
        for ask in &req.asks {
            let name = ask.name.clone();
            let outcome = match &reply {
                Err(_) => Outcome::TimedOut,
                Ok(None) => {
                    note = Some(
                        "No one was available to enter it (this run isn't interactive). \
                         Don't ask for it in the chat.",
                    );
                    Outcome::Skipped
                }
                Ok(Some(values)) => match values.get(&name) {
                    Some(raw) => {
                        // Keys get the usual paste clean-up; anything else
                        // (a password with spaces) is only trimmed.
                        let value = normalize_key(raw).unwrap_or_else(|_| raw.trim().to_owned());
                        if value.is_empty() {
                            Outcome::Skipped
                        } else if let Err(e) = self.secrets.set(&name, &value) {
                            return ToolResult::error(format!("saving {name}: {e}"));
                        } else {
                            tracing::info!(secret = %name, "secret set from a secret_request");
                            Outcome::Set
                        }
                    }
                    None => Outcome::Skipped,
                },
            };
            if outcome == Outcome::Skipped {
                self.with_run(&ctx.run_id, |r| r.skipped.insert(name.clone()));
            }
            outcomes.insert(name, outcome);
        }
        result(&outcomes, note)
    }
}

#[cfg(test)]
mod tests;
