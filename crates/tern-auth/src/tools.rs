//! The agent's side of signing in (tooling spec §6.7): `auth_discover`
//! finds how a service signs in; `auth_propose` asks the user to choose;
//! `connection_add` opens the sign-in card. The model sees plans and
//! outcomes, never a token, and can't change a plan after the user saw it:
//! plans are kept here by id.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult};
use tern_core::types::ToolSpec;

use crate::discover::{self, AuthPlan, Clients};
use crate::http::Guard;

/// Plans kept for this many minutes.
const PLAN_TTL_MS: i64 = 60 * 60 * 1000;
const MAX_PLANS: usize = 200;

/// A plan as the host keeps it.
#[derive(Debug, Clone)]
pub struct Kept {
    pub plan: AuthPlan,
    /// What the user chose on the `auth_propose` card: `oauth` or `key`.
    pub chosen: Option<String>,
    at: i64,
}

/// Plans by id, shared by the auth tools.
#[derive(Default)]
pub struct Plans(Mutex<HashMap<String, Kept>>);

impl Plans {
    pub fn keep(&self, plan: AuthPlan) -> String {
        let id = format!("plan_{}", crate::pkce::random(9));
        let now = tern_store::now_ms();
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, k| now - k.at < PLAN_TTL_MS);
        if m.len() >= MAX_PLANS
            && let Some(oldest) = m.iter().min_by_key(|(_, k)| k.at).map(|(id, _)| id.clone())
        {
            m.remove(&oldest);
        }
        m.insert(
            id.clone(),
            Kept {
                plan,
                chosen: None,
                at: now,
            },
        );
        id
    }

    pub fn get(&self, id: &str) -> Option<Kept> {
        let m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.get(id)
            .filter(|k| tern_store::now_ms() - k.at < PLAN_TTL_MS)
            .cloned()
    }

    pub fn choose(&self, id: &str, choice: &str) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(k) = m.get_mut(id) {
            k.chosen = Some(choice.to_owned());
        }
    }
}

/// What the auth tools share.
pub struct AuthHost {
    pub guard: Guard,
    pub clients: Clients,
    pub plans: Plans,
}

impl AuthHost {
    pub fn new(cfg: &tern_core::config::AuthConfig) -> Self {
        Self {
            guard: Guard::new(&cfg.local_issuers),
            clients: Clients {
                configured: cfg.clients.keys().cloned().collect(),
                cimd: !cfg.client_metadata_url.is_empty(),
            },
            plans: Plans::default(),
        }
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The plan as the model sees it: everything but internals, plus its id
/// and what the host recommends.
pub fn plan_json(id: &str, plan: &AuthPlan) -> Value {
    let mut v = serde_json::to_value(plan).unwrap_or_default();
    v["plan_id"] = Value::String(id.to_owned());
    v["recommended"] = Value::String(recommend(plan).to_owned());
    v
}

/// §6.7.3: OAuth when the user can sign in without setup; OAuth with setup
/// steps when they must register an app; a key first when OAuth needs a
/// secret a desktop app can't keep.
pub fn recommend(plan: &AuthPlan) -> &'static str {
    if !plan.is_oauth() || (plan.confidential && !plan.client_ready) {
        "key"
    } else {
        "oauth"
    }
}

pub struct AuthDiscover {
    pub host: Arc<AuthHost>,
}

pub const DISCOVER: &str = "auth_discover";

#[async_trait]
impl Tool for AuthDiscover {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: DISCOVER.into(),
            description: "Find out how a service wants to be signed into, before writing a connector or adding an MCP server for it. Checks tern's built-in profiles (Google, Microsoft, GitHub, Slack, Notion, Linear, Atlassian, Dropbox), the MCP authorization metadata of an MCP server URL, and the OAuth metadata (RFC 8414, OpenID) of an API's host. Returns a structured plan with a plan_id: method \"oauth2\" (with endpoints, whether a client ID needs setup, and scopes) or \"unknown\" (then research the service's docs and pass what you find to auth_propose, or fall back to an API key). Read-only.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "service": {"type": "string", "description": "The service's name, e.g. \"Gmail\" or \"Notion\""},
                    "url": {"type": "string", "description": "The API base URL or the MCP server URL, when known"},
                    "mcp": {"type": "boolean", "description": "url is a remote MCP server"}
                },
                "required": ["service"]
            }),
        }
    }

    fn effect(&self) -> Effect {
        Effect::Read
    }

    fn egress_host(&self, args: &Value) -> Option<String> {
        let url = reqwest::Url::parse(str_arg(args, "url")?).ok()?;
        url.host_str().map(str::to_ascii_lowercase)
    }

    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }

    fn summary(&self, args: &Value) -> Option<String> {
        Some(format!("auth_discover {}", str_arg(args, "service")?))
    }

    async fn call(&self, _: &ToolCtx, args: Value) -> ToolResult {
        let Some(service) = str_arg(&args, "service") else {
            return ToolResult::error("invalid arguments: `service` must be a string");
        };
        let url = str_arg(&args, "url");
        let mcp = args.get("mcp").and_then(Value::as_bool).unwrap_or(false);
        match discover::discover(&self.host.guard, &self.host.clients, service, url, mcp).await {
            Ok(plan) => {
                let id = self.host.plans.keep(plan.clone());
                let mut out = plan_json(&id, &plan);
                out["next"] = Value::String(next_step(&plan).into());
                ToolResult::ok(serde_json::to_string_pretty(&out).unwrap_or_default())
            }
            Err(e) => ToolResult::error(format!("auth_discover: {e}")),
        }
    }
}

fn next_step(plan: &AuthPlan) -> &'static str {
    if plan.is_oauth() {
        "Call auth_propose with this plan_id so the user chooses how to connect."
    } else {
        "No OAuth found. Search the service's developer docs for \"OAuth\"; if it has OAuth, call auth_propose with a researched plan (authorize_url, token_url, api_hosts and the doc URLs as evidence). Otherwise call auth_propose with method \"key\"."
    }
}
