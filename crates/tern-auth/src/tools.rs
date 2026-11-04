//! The agent's side of signing in (tooling spec §6.7): `auth_discover`
//! finds how a service signs in; `auth_propose` asks the user to choose;
//! `connection_add` opens the sign-in card. The model sees plans and
//! outcomes, never a token, and can't change a plan after the user saw it:
//! plans are kept here by id.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tern_core::approval::{
    ApprovalHandler, AuthCard, AuthOption, AuthPrompt, SecretAsk, SecretRequest,
};
use tern_core::config::AuthConfig;
use tern_core::tool::{Effect, Tool, ToolCtx, ToolPolicy, ToolResult, current_call_id};
use tern_core::types::ToolSpec;

use crate::connections::{Connections, Kind};
use crate::discover::{self, AuthPlan, ClientKind, Clients, Source};
use crate::error::{AuthError, short};
use crate::flow::OAuthConfig;
use crate::http::Guard;
use crate::signin::{self, Answer, ClientSource, Recorded, Shown};

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

/// A connector or MCP server accounts can be added to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub kind: Kind,
    pub name: String,
    /// Secrets it names (`capabilities.auth`, an MCP server's secret
    /// references): what a key-based account must provide.
    pub key_names: Vec<String>,
    /// `capabilities.oauth`: provider and scopes.
    pub oauth: Option<(String, Vec<String>)>,
}

/// Every target, computed when asked (connectors written moments ago count).
pub type Targets = Arc<dyn Fn() -> Vec<Target> + Send + Sync>;

/// What the auth tools share.
pub struct AuthHost {
    pub guard: Guard,
    pub clients: Clients,
    pub plans: Plans,
    pub config: AuthConfig,
}

impl AuthHost {
    pub fn new(cfg: &AuthConfig) -> Self {
        Self {
            guard: Guard::new(&cfg.local_issuers),
            clients: Clients {
                configured: cfg.clients.keys().cloned().collect(),
                cimd: !cfg.client_metadata_url.is_empty(),
            },
            plans: Plans::default(),
            config: cfg.clone(),
        }
    }

    /// Fill in a client for `cfg`: the user's configured app, a client ID
    /// metadata document, or registration. `Err` says what to set up.
    pub fn client(
        &self,
        cfg: &mut OAuthConfig,
        kind: Option<ClientKind>,
    ) -> Result<ClientSource, String> {
        if !cfg.client_id.is_empty() {
            return Ok(ClientSource::Ready);
        }
        if let Some(c) = self.config.clients.get(&cfg.provider) {
            cfg.client_id = c.client_id.clone();
            cfg.client_secret_secret = c.client_secret_secret.clone();
            if cfg.client_secret_secret.is_some() && cfg.token_auth == crate::TokenAuth::None {
                cfg.token_auth = crate::TokenAuth::Post;
            }
            return Ok(ClientSource::Ready);
        }
        if kind == Some(ClientKind::Cimd) && !self.config.client_metadata_url.is_empty() {
            cfg.client_id = self.config.client_metadata_url.clone();
            return Ok(ClientSource::Ready);
        }
        if let Some(r) = &cfg.registration_url {
            return Ok(ClientSource::Register(r.clone()));
        }
        let setup = crate::profiles::by_id(&cfg.provider)
            .map(|p| format!(" (register one at {})", p.setup_url))
            .unwrap_or_default();
        Err(format!(
            "no OAuth app for {p}: the user registers one{setup} and adds it to config.toml as [auth.clients] {p} = {{ client_id = \"…\" }}; or use a key instead",
            p = cfg.provider
        ))
    }

    /// The OAuth config for a target with no plan: an existing account's,
    /// else the built-in profile its `capabilities.oauth` names.
    pub fn config_for(
        &self,
        conns: &Connections,
        target: &Target,
    ) -> Option<(OAuthConfig, Option<ClientKind>)> {
        if let Some(c) = conns
            .list(target.kind, &target.name)
            .into_iter()
            .find_map(|c| c.auth.oauth().cloned())
        {
            return Some((c, None));
        }
        let (provider, scopes) = target.oauth.as_ref()?;
        let p = crate::profiles::by_id(provider)?;
        let plan = discover::from_profile(p, &self.clients, Vec::new());
        Some((
            plan.oauth_config("", merge_scopes(&plan.scopes_supported, scopes))?,
            plan.client,
        ))
    }
}

fn merge_scopes(base: &[String], wanted: &[String]) -> Vec<String> {
    let mut v: Vec<String> = base.to_vec();
    for s in wanted {
        if !v.contains(s) {
            v.push(s.clone());
        }
    }
    v
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

// ---- auth_propose ----

pub const PROPOSE: &str = "auth_propose";

pub struct AuthPropose {
    pub host: Arc<AuthHost>,
    pub asker: Arc<dyn ApprovalHandler>,
}

/// Where a plan came from, in plain words, for the card.
fn provenance(plan: &AuthPlan) -> String {
    let host = |u: &Option<String>| {
        u.as_deref()
            .and_then(|u| reqwest::Url::parse(u).ok())
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default()
    };
    match plan.source {
        Some(Source::Builtin) => format!("From tern's built-in {} profile.", plan.service),
        Some(Source::McpMetadata) | Some(Source::WellKnown) => {
            format!("Found via {} metadata (verified).", host(&plan.issuer))
        }
        Some(Source::Research) if plan.verified => format!(
            "From the service's docs, confirmed by {} metadata.",
            host(&plan.issuer)
        ),
        Some(Source::Research) => {
            "Unverified: from the service's docs; the service publishes no metadata to confirm these addresses.".into()
        }
        _ => String::new(),
    }
}

/// The card for an OAuth plan (§6.7.3): options, recommendation and notes.
pub fn choose_card(plan: &AuthPlan) -> AuthCard {
    let rec = recommend(plan);
    let oauth_detail = if plan.no_setup() {
        "No keys to copy: you sign in and choose what tern can see.".to_owned()
    } else if plan.confidential {
        format!(
            "Needs an OAuth app with a client secret{}.",
            plan.setup_url
                .as_deref()
                .map(|u| format!(" ({u})"))
                .unwrap_or_default()
        )
    } else if !plan.client_ready {
        format!(
            "Needs your own OAuth app first{}; then you sign in.",
            plan.setup_url
                .as_deref()
                .map(|u| format!(" ({u})"))
                .unwrap_or_default()
        )
    } else {
        "You sign in in your browser; tern keeps the tokens.".to_owned()
    };
    let service = short(&plan.service, 40);
    let mut notes = vec![provenance(plan)];
    if let Some(a) = plan
        .authorize_url
        .as_deref()
        .and_then(|u| reqwest::Url::parse(u).ok())
        .and_then(|u| u.host_str().map(str::to_owned))
    {
        notes.push(format!("Sign-in page: {a}"));
    }
    if !plan.scopes_supported.is_empty() {
        notes.push(format!("Scopes: {}", plan.scopes_supported.join(", ")));
    }
    notes.extend(plan.notes.iter().cloned());
    notes.retain(|n| !n.is_empty());
    AuthCard::Choose {
        title: format!("Connect {service}"),
        options: vec![
            AuthOption {
                id: "oauth".into(),
                label: format!("Sign in with {service}"),
                detail: oauth_detail,
                recommended: rec == "oauth",
            },
            AuthOption {
                id: "key".into(),
                label: "Use an API key or token instead".into(),
                detail: "You paste it into a private prompt; the agent never sees it.".into(),
                recommended: rec == "key",
            },
        ],
        notes,
    }
}

fn prompt(ctx: &ToolCtx, card: AuthCard) -> AuthPrompt {
    AuthPrompt {
        id: format!("auth_{}", crate::pkce::random(9)),
        run_id: ctx.run_id.clone(),
        session_id: ctx.session_id.clone(),
        call_id: current_call_id(),
        parent_run_id: None,
        agent_label: None,
        card,
    }
}

#[async_trait]
impl Tool for AuthPropose {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: PROPOSE.into(),
            description: "Ask the user how to connect a service: shows a card with signing in (OAuth) and an API key as options, the host's recommendation, and where the sign-in addresses came from. The user chooses; you never pick for them. Pass plan_id from auth_discover; or, when discovery said \"unknown\" and you found OAuth in the service's docs, pass `plan` with authorize_url, token_url, api_hosts (the API hosts the connector will call) and evidence (the doc URLs), which tern checks against the API's domain and the issuer's metadata. Returns {\"choice\": \"oauth\" | \"key\" | \"cancel\", \"plan_id\"}.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "plan_id": {"type": "string", "description": "From auth_discover"},
                    "plan": {
                        "type": "object",
                        "description": "A researched plan, when auth_discover found nothing",
                        "properties": {
                            "service": {"type": "string"},
                            "authorize_url": {"type": "string"},
                            "token_url": {"type": "string"},
                            "device_url": {"type": "string"},
                            "revocation_url": {"type": "string"},
                            "userinfo_url": {"type": "string"},
                            "issuer": {"type": "string"},
                            "scopes_supported": {"type": "array", "items": {"type": "string"}},
                            "api_hosts": {"type": "array", "items": {"type": "string"}},
                            "evidence": {"type": "array", "items": {"type": "string"}}
                        },
                        "required": ["service", "authorize_url", "token_url", "api_hosts"]
                    }
                }
            }),
        }
    }

    fn effect(&self) -> Effect {
        Effect::Read
    }

    /// The card is the user's own decision.
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::new(Effect::Read, true)
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let (id, plan) = if let Some(id) = str_arg(&args, "plan_id") {
            match self.host.plans.get(id) {
                Some(k) => (id.to_owned(), k.plan),
                None => {
                    return ToolResult::error(format!(
                        "no plan {id:?} (plans last an hour); call auth_discover again"
                    ));
                }
            }
        } else if let Some(raw) = args.get("plan") {
            let mut plan: AuthPlan = match serde_json::from_value(raw.clone()) {
                Ok(p) => p,
                Err(e) => return ToolResult::error(format!("invalid plan: {e}")),
            };
            // Only the addresses and evidence are the agent's; what the
            // host derives (verification, client, provenance) is reset.
            plan.method = "oauth2".into();
            plan.client = None;
            plan.client_ready = false;
            plan.confidential = false;
            plan.registration_url = None;
            plan.resource = None;
            plan.token_auth = None;
            plan.extra_params.clear();
            plan.notes.clear();
            plan.provider.clear();
            plan.service = short(&plan.service, 40);
            if let Err(e) = discover::validate_research(&self.host.guard, &mut plan).await {
                return ToolResult::error(format!(
                    "auth_propose: this plan can't be used: {e}. Offer the key flow instead (secret_request), or ask the user for the service's official docs."
                ));
            }
            plan.client = Some(ClientKind::UserSupplied);
            plan.client_ready = self.host.clients.configured.contains(&plan.provider);
            (self.host.plans.keep(plan.clone()), plan)
        } else {
            return ToolResult::error("pass plan_id (from auth_discover) or a researched plan");
        };
        if !plan.is_oauth() {
            self.host.plans.choose(&id, "key");
            return ToolResult::ok(
                json!({"plan_id": id, "choice": "key", "why": "no OAuth: use a key"}).to_string(),
            );
        }
        let req = prompt(ctx, choose_card(&plan));
        let answer = tokio::select! {
            a = self.asker.ask_auth(&req) => a,
            () = ctx.cancel.cancelled() => return ToolResult::error("cancelled"),
        };
        let Some(choice) = answer.filter(|a| req.accepts(a)) else {
            return ToolResult::ok(
                json!({"plan_id": id, "choice": "none",
                       "why": "nobody was available to choose; tell the user how they can connect it"})
                .to_string(),
            );
        };
        self.host.plans.choose(&id, &choice);
        let next = match choice.as_str() {
            "oauth" => format!(
                "Build the connector with capabilities.oauth = {{ {} = {{ scopes = {{ … }} }} }} (or add the MCP server with no auth fields), then call connection_add with plan_id.",
                plan.provider
            ),
            "key" => "Build the connector with capabilities.auth = \"NAME\" (or the MCP server's secret fields), then call connection_add with method \"key\".".into(),
            _ => "The user cancelled. Don't connect it.".into(),
        };
        ToolResult::ok(json!({"plan_id": id, "choice": choice, "next": next}).to_string())
    }
}

// ---- connection_add ----

pub const ADD: &str = "connection_add";
/// Sign-in cards one run may open.
const MAX_SIGN_INS_PER_RUN: u32 = 3;

pub struct ConnectionAdd {
    pub host: Arc<AuthHost>,
    pub conns: Arc<Connections>,
    pub asker: Arc<dyn ApprovalHandler>,
    pub targets: Targets,
    runs: Mutex<HashMap<String, u32>>,
}

/// The target named `name` (of `kind`, when both exist).
pub fn find_target(targets: &Targets, name: &str, kind: Option<&str>) -> Result<Target, String> {
    let all: Vec<Target> = targets()
        .into_iter()
        .filter(|t| t.name == name && kind.is_none_or(|k| t.kind.as_str() == k))
        .collect();
    match all.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(format!(
            "no connector or MCP server named {name:?}; write or add it first"
        )),
        _ => Err(format!(
            "both a connector and an MCP server are named {name:?}; pass kind"
        )),
    }
}

impl ConnectionAdd {
    pub fn new(
        host: Arc<AuthHost>,
        conns: Arc<Connections>,
        asker: Arc<dyn ApprovalHandler>,
        targets: Targets,
    ) -> Self {
        Self {
            host,
            conns,
            asker,
            targets,
            runs: Mutex::default(),
        }
    }

    fn target(&self, name: &str, kind: Option<&str>) -> Result<Target, String> {
        find_target(&self.targets, name, kind)
    }
}

fn outcome(status: &str, extra: Value) -> ToolResult {
    let mut v = json!({"status": status});
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    ToolResult::ok(v.to_string())
}

#[async_trait]
impl Tool for ConnectionAdd {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ADD.into(),
            description: "Connect an account to a connector or MCP server: opens a \"Sign in to …\" card for the user (OAuth in their browser, or a device code), or a private prompt for a key. You never see a token or key. Use it after auth_propose (pass plan_id) or to add another account (several Gmail accounts, say): `connection` names the account (\"work\", \"personal\"; made from the account's email if left out). Returns {\"status\": \"connected\" | \"reconnected\" | \"skipped\" | \"timed_out\", \"connection\", \"account\"}; signing in to an account that is already connected refreshes it instead of adding a duplicate.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "target": {"type": "string", "description": "The connector's or MCP server's name"},
                    "kind": {"type": "string", "enum": ["connector", "mcp"], "description": "Only when a connector and an MCP server share the name"},
                    "connection": {"type": "string", "description": "The account's short name: [a-z0-9_-], up to 16"},
                    "label": {"type": "string", "description": "A display name for the account"},
                    "method": {"type": "string", "enum": ["oauth", "key"], "description": "Default: what the user chose on auth_propose, else OAuth when the connector declares capabilities.oauth"},
                    "plan_id": {"type": "string", "description": "From auth_discover / auth_propose"}
                },
                "required": ["target"]
            }),
        }
    }

    fn effect(&self) -> Effect {
        Effect::Write
    }

    /// The card is the user's gate: they sign in or not.
    fn policy(&self, _: &ToolCtx, _: &Value) -> ToolPolicy {
        ToolPolicy::granted_write(true)
    }

    fn summary(&self, args: &Value) -> Option<String> {
        Some(format!(
            "connection_add {}{}",
            str_arg(args, "target")?,
            str_arg(args, "connection")
                .map(|c| format!(" · {c}"))
                .unwrap_or_default()
        ))
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> ToolResult {
        let Some(name) = str_arg(&args, "target") else {
            return ToolResult::error("invalid arguments: `target` must be a string");
        };
        let target = match self.target(name, str_arg(&args, "kind")) {
            Ok(t) => t,
            Err(e) => return ToolResult::error(e),
        };
        let slug = str_arg(&args, "connection").map(str::to_ascii_lowercase);
        let label = str_arg(&args, "label").map(|l| short(l, 60));
        if let Some(s) = &slug
            && !tern_core::secrets::valid_slug(s)
        {
            return ToolResult::error(format!(
                "connection names are [a-z0-9_-], 1 to 16 characters, not {s:?}"
            ));
        }
        let kept = str_arg(&args, "plan_id").and_then(|id| self.host.plans.get(id));
        if str_arg(&args, "plan_id").is_some() && kept.is_none() {
            return ToolResult::error(
                "that plan_id is unknown or expired; call auth_discover again",
            );
        }
        let method = str_arg(&args, "method")
            .map(str::to_owned)
            .or_else(|| kept.as_ref().and_then(|k| k.chosen.clone()))
            .unwrap_or_else(|| {
                let has_oauth = kept.as_ref().is_some_and(|k| k.plan.is_oauth())
                    || target.oauth.is_some()
                    || self
                        .conns
                        .list(target.kind, &target.name)
                        .iter()
                        .any(|c| c.auth.oauth().is_some());
                if has_oauth { "oauth" } else { "key" }.to_owned()
            });
        if kept
            .as_ref()
            .and_then(|k| k.chosen.as_deref())
            .is_some_and(|c| c != method)
        {
            return ToolResult::error(format!(
                "the user chose {:?} on the auth_propose card, not {method:?}",
                kept.as_ref()
                    .and_then(|k| k.chosen.clone())
                    .unwrap_or_default()
            ));
        }
        let over = {
            let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
            if runs.len() > 1000 {
                runs.clear();
            }
            let n = runs.entry(ctx.run_id.clone()).or_default();
            *n += 1;
            *n > MAX_SIGN_INS_PER_RUN
        };
        if over {
            return ToolResult::error(
                "this run has opened enough sign-in cards; tell the user what is left to connect",
            );
        }
        if let Some(s) = &slug
            && self.conns.get(target.kind, &target.name, s).is_none()
            && let Err(e) = self.conns.check_new(target.kind, &target.name, s)
        {
            return ToolResult::error(e);
        }
        match method.as_str() {
            "oauth" => {
                self.oauth(ctx, &target, slug.as_deref(), label.as_deref(), kept)
                    .await
            }
            "key" => {
                self.key(ctx, &target, slug.as_deref(), label.as_deref())
                    .await
            }
            m => ToolResult::error(format!("method must be \"oauth\" or \"key\", not {m:?}")),
        }
    }
}

/// Why a connection wasn't made.
#[derive(Debug)]
pub enum ConnectError {
    /// The user cancelled, declined, or nobody was there.
    Skipped(String),
    TimedOut,
    Failed(String),
}

/// Present a sign-in through cards from `asker` for `run`, `session` and
/// tool call `call_id` (all empty for one a front end started).
pub fn card_presenter(
    asker: Arc<dyn ApprovalHandler>,
    run: String,
    session: String,
    call_id: Option<String>,
    shown_name: String,
) -> impl FnMut(Shown) -> Answer + Send {
    move |shown: Shown| -> Answer {
        let card = match shown {
            Shown::Url {
                url,
                device_available,
            } => AuthCard::SignIn {
                title: format!("Sign in to {shown_name}"),
                url: Some(url),
                user_code: None,
                verification_uri: None,
                device_available,
                notes: vec!["Opens the service's own sign-in page in your browser.".into()],
            },
            Shown::Device {
                user_code,
                verification_uri,
                ..
            } => AuthCard::SignIn {
                title: format!("Sign in to {shown_name}"),
                url: None,
                user_code: Some(user_code),
                verification_uri: Some(verification_uri),
                device_available: false,
                notes: vec!["Enter the code on the service's page, from any device.".into()],
            },
        };
        let req = AuthPrompt {
            id: format!("auth_{}", crate::pkce::random(9)),
            run_id: run.clone(),
            session_id: session.clone(),
            call_id: call_id.clone(),
            parent_run_id: None,
            agent_label: None,
            card,
        };
        let asker = asker.clone();
        Box::pin(async move {
            match asker.ask_auth(&req).await {
                Some(a) if req.accepts(&a) => Some(a),
                Some(_) => Some("cancel".into()),
                // Nobody to show it to: give up rather than wait.
                None => Some("nobody".into()),
            }
        })
    }
}

/// Sign in an account of `target` with OAuth and record it: the shared
/// path of `connection_add`, the API and the CLI. The config is the plan
/// the user chose from, an existing account's, or the built-in profile the
/// connector names.
/// Which account to sign in, and how.
pub struct Connect<'a> {
    pub target: &'a Target,
    /// The account's slug; made from the account when `None`.
    pub slug: Option<&'a str>,
    pub label: Option<&'a str>,
    /// The plan the user chose from, if any.
    pub kept: Option<&'a Kept>,
    /// A device code instead of the browser on this machine.
    pub device: bool,
}

pub async fn connect_oauth(
    host: &AuthHost,
    conns: &Connections,
    req: Connect<'_>,
    present: impl FnMut(Shown) -> Answer,
) -> Result<Recorded, ConnectError> {
    let Connect {
        target,
        slug,
        label,
        kept,
        device: prefer_device,
    } = req;
    let (mut cfg, kind) = match kept {
        Some(k) => {
            let scopes = match &target.oauth {
                Some((_, s)) => merge_scopes(&k.plan.scopes_supported_base(), s),
                None => k.plan.scopes_supported.clone(),
            };
            let c = k
                .plan
                .oauth_config("", scopes)
                .ok_or_else(|| ConnectError::Failed("that plan isn't an OAuth plan".into()))?;
            (c, k.plan.client)
        }
        None => host.config_for(conns, target).ok_or_else(|| {
            ConnectError::Failed(format!(
                "{} has no OAuth sign-in tern knows: call auth_discover and auth_propose first, or use a key",
                target.name
            ))
        })?,
    };
    if let Some((provider, _)) = &target.oauth
        && *provider != cfg.provider
    {
        return Err(ConnectError::Failed(format!(
            "connector {} declares capabilities.oauth {provider:?}, but the plan signs in with {:?}",
            target.name, cfg.provider
        )));
    }
    // Registration is per account: a new account doesn't reuse another's
    // registered client (the user's configured app is shared).
    let reusing = conns
        .list(target.kind, &target.name)
        .iter()
        .filter(|c| Some(c.slug.as_str()) != slug)
        .any(|c| c.auth.oauth().is_some_and(|o| o.client_id == cfg.client_id));
    if reusing && cfg.registration_url.is_some() && !host.config.clients.contains_key(&cfg.provider)
    {
        cfg.client_id.clear();
        cfg.redirect_uri = None;
    }
    // Signing in again keeps that account's own registered client.
    if let Some(own) = slug
        .and_then(|s| conns.get(target.kind, &target.name, s))
        .and_then(|c| c.auth.oauth().cloned())
    {
        cfg.client_id = own.client_id;
        cfg.redirect_uri = own.redirect_uri;
        cfg.token_auth = own.token_auth;
    }
    let client = host.client(&mut cfg, kind).map_err(ConnectError::Failed)?;
    let timeout = std::time::Duration::from_secs(host.config.sign_in_timeout_secs.max(10));
    let signed = match signin::oauth(conns, cfg, client, prefer_device, timeout, present).await {
        Ok(s) => s,
        Err(AuthError::Cancelled) => {
            return Err(ConnectError::Skipped(
                "the user cancelled the sign-in, or nobody was there to sign in".into(),
            ));
        }
        Err(AuthError::TimedOut) => return Err(ConnectError::TimedOut),
        Err(AuthError::Denied) => {
            return Err(ConnectError::Skipped(
                "the user declined at the provider".into(),
            ));
        }
        Err(e) => return Err(ConnectError::Failed(e.to_string())),
    };
    signin::record_oauth(conns, target.kind, &target.name, slug, label, signed)
        .map_err(ConnectError::Failed)
}

impl ConnectionAdd {
    async fn oauth(
        &self,
        ctx: &ToolCtx,
        target: &Target,
        slug: Option<&str>,
        label: Option<&str>,
        kept: Option<Kept>,
    ) -> ToolResult {
        let shown_name = match slug {
            Some(s) => format!("{} · {s}", target.name),
            None => target.name.clone(),
        };
        let present = card_presenter(
            self.asker.clone(),
            ctx.run_id.clone(),
            ctx.session_id.clone(),
            current_call_id(),
            shown_name,
        );
        let req = Connect {
            target,
            slug,
            label,
            kept: kept.as_ref(),
            device: false,
        };
        let run = connect_oauth(&self.host, &self.conns, req, present);
        let r = tokio::select! {
            r = run => r,
            () = ctx.cancel.cancelled() => return ToolResult::error("cancelled"),
        };
        match r {
            Ok(r) => recorded(&r),
            Err(ConnectError::Skipped(why)) => outcome(
                "skipped",
                json!({"why": format!("{why}; don't ask for a key in the chat")}),
            ),
            Err(ConnectError::TimedOut) => outcome("timed_out", json!({})),
            Err(ConnectError::Failed(e)) => ToolResult::error(format!("connection_add: {e}")),
        }
    }

    async fn key(
        &self,
        ctx: &ToolCtx,
        target: &Target,
        slug: Option<&str>,
        label: Option<&str>,
    ) -> ToolResult {
        if target.key_names.is_empty() {
            return ToolResult::error(format!(
                "{} names no secrets (capabilities.auth, or the MCP server's secret fields), so there is no key to ask for",
                target.name
            ));
        }
        let slug = slug
            .map(str::to_owned)
            .unwrap_or_else(|| signin::new_slug(&self.conns, target.kind, &target.name, None));
        let key = crate::ConnKey::new(&target.name, &slug);
        let secrets = self.conns.tokens.secrets();
        let asks: Vec<SecretAsk> = target
            .key_names
            .iter()
            .map(|n| {
                let name = key.secret_name(n);
                SecretAsk {
                    in_file: secrets.get(&name).is_some(),
                    in_env: false,
                    used_by: vec![format!("{} {} · {slug}", target.kind.as_str(), target.name)],
                    name,
                }
            })
            .collect();
        let req = SecretRequest {
            id: format!("sec_{}", crate::pkce::random(9)),
            run_id: ctx.run_id.clone(),
            session_id: ctx.session_id.clone(),
            call_id: current_call_id(),
            asks,
            reason: format!("The key for {} · {slug}", target.name),
            replace: true,
            parent_run_id: None,
            agent_label: None,
        };
        let timeout = std::time::Duration::from_secs(self.host.config.sign_in_timeout_secs.max(10));
        let reply = tokio::select! {
            r = tokio::time::timeout(timeout, self.asker.ask_secrets(&req)) => r,
            () = ctx.cancel.cancelled() => return ToolResult::error("cancelled"),
        };
        let values = match reply {
            Err(_) => return outcome("timed_out", json!({})),
            Ok(None) => {
                return outcome(
                    "skipped",
                    json!({"why": "nobody was available to enter it; don't ask for it in the chat"}),
                );
            }
            Ok(Some(v)) => v,
        };
        let plain: std::collections::BTreeMap<String, String> = target
            .key_names
            .iter()
            .filter_map(|n| {
                values
                    .get(&key.secret_name(n))
                    .map(|v| (n.clone(), v.clone()))
            })
            .collect();
        match connect_key(&self.conns, target, &slug, label, &plain) {
            Ok(r) => recorded(&r),
            Err(ConnectError::Skipped(why)) => outcome(
                "skipped",
                json!({"why": format!("{why}; don't ask for it in the chat")}),
            ),
            Err(ConnectError::TimedOut) => outcome("timed_out", json!({})),
            Err(ConnectError::Failed(e)) => ToolResult::error(format!("connection_add: {e}")),
        }
    }
}

/// Store a key-based account's secrets (by the names the target declares)
/// under its connection and record it.
pub fn connect_key(
    conns: &Connections,
    target: &Target,
    slug: &str,
    label: Option<&str>,
    values: &std::collections::BTreeMap<String, String>,
) -> Result<Recorded, ConnectError> {
    let key = crate::ConnKey::new(&target.name, slug);
    let mut clean = Vec::new();
    for name in &target.key_names {
        let Some(raw) = values.get(name) else {
            return Err(ConnectError::Skipped(format!("{name} wasn't entered")));
        };
        let value =
            tern_core::secrets::keys::normalize_key(raw).unwrap_or_else(|_| raw.trim().to_owned());
        if value.is_empty() {
            return Err(ConnectError::Skipped(format!("{name} was empty")));
        }
        clean.push((key.secret_name(name), value));
    }
    if let Some(extra) = values.keys().find(|k| !target.key_names.contains(k)) {
        return Err(ConnectError::Failed(format!(
            "{} doesn't use a secret named {extra}",
            target.name
        )));
    }
    let pairs: Vec<(&str, &str)> = clean
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    conns
        .tokens
        .secrets()
        .set_many(&pairs)
        .map_err(|e| ConnectError::Failed(format!("saving the key: {e}")))?;
    signin::record_key(
        conns,
        target.kind,
        &target.name,
        slug,
        label,
        target.key_names.clone(),
    )
    .map_err(ConnectError::Failed)
}

fn recorded(r: &Recorded) -> ToolResult {
    let c = r.conn();
    let status = match r {
        Recorded::Connected(_) => "connected",
        Recorded::Reconnected(_) => "reconnected",
    };
    outcome(
        status,
        json!({"connection": c.slug, "account": c.account, "default": c.is_default,
               "next": "Test it with connector_test or a read-only call, passing `connection` when there is more than one account."}),
    )
}

impl AuthPlan {
    /// The scopes a profile always adds (`openid email` for Google), for a
    /// connector that names its own.
    fn scopes_supported_base(&self) -> Vec<String> {
        if self.source == Some(Source::Builtin) {
            self.scopes_supported.clone()
        } else {
            Vec::new()
        }
    }
}
