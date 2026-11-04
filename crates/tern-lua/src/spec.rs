//! Validating the table a connector file returns (spec §10.2).

use mlua::{Function, Lua, LuaSerdeExt, Table, Value};
use serde::{Deserialize, Serialize};
use tern_core::config::Rate;
use tern_core::tool::Effect;

use crate::host::LoadError;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Hosts `ctx.http` may reach (`api.example.com` or `*.example.com`),
    /// each optionally limited to paths (`api.example.com/v1/*`).
    pub http: Vec<String>,
    /// Secret names `ctx.secret` may read: the legacy way, since the
    /// script sees the value. New connectors use `auth`.
    pub secrets: Vec<String>,
    /// Credentials the host adds to requests; the script never sees them.
    pub auth: Vec<Auth>,
    /// Requests per period, shared by every call of this connector. `None`
    /// uses the host's default (`[connectors] rate`).
    pub rate: Option<Rate>,
    /// Signs in with OAuth (tooling spec §6.2): tokens come from the
    /// connection the call runs under and are sent by the host.
    pub oauth: Option<OAuthCap>,
}

/// `capabilities.oauth = { google = { scopes = { … } } }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthCap {
    /// A built-in profile's id or a discovered provider's name.
    pub provider: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// The credential name an `auth` entry uses for OAuth tokens.
pub fn oauth_secret(provider: &str) -> String {
    format!("oauth:{provider}")
}

/// Whether a credential name is an OAuth source rather than a secret.
pub fn is_oauth_secret(name: &str) -> bool {
    name.starts_with("oauth:")
}

impl Capabilities {
    /// Secret names the host injects through `auth`.
    pub fn auth_secrets(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .auth
            .iter()
            .flat_map(Auth::secrets)
            .map(str::to_owned)
            .collect();
        v.dedup();
        v
    }

    /// Every secret this connector uses, raw or injected (OAuth tokens are
    /// the connection's, not named secrets).
    pub fn all_secrets(&self) -> Vec<String> {
        let mut v = self.secrets.clone();
        for s in self
            .auth_secrets()
            .into_iter()
            .filter(|s| !is_oauth_secret(s))
        {
            if !v.contains(&s) {
                v.push(s);
            }
        }
        v
    }

    /// Reads secret values itself (`ctx.secret`), the pre-T4b way.
    pub fn legacy_secrets(&self) -> bool {
        !self.secrets.is_empty()
    }

    /// One line per capability, for summaries and for the diff shown when
    /// a connector is replaced. `default_rate` is shown when none is declared.
    pub fn lines(&self, default_rate: Rate) -> Vec<String> {
        let mut out: Vec<String> = self.http.iter().map(|h| format!("http {h}")).collect();
        if let Some(o) = &self.oauth {
            out.push(format!("oauth {}", o.describe()));
        }
        for a in &self.auth {
            out.push(format!("auth {}", a.describe()));
        }
        for s in &self.secrets {
            out.push(format!(
                "raw secret {s} (legacy ctx.secret: the script sees the value)"
            ));
        }
        out.push(match self.rate {
            Some(r) => format!("rate {r}"),
            None => format!("rate {default_rate} (default)"),
        });
        out
    }
}

impl OAuthCap {
    /// `google (scopes: gmail.readonly, gmail.send)`.
    pub fn describe(&self) -> String {
        if self.scopes.is_empty() {
            self.provider.clone()
        } else {
            format!("{} (scopes: {})", self.provider, self.scopes.join(", "))
        }
    }
}

/// Where the host puts a credential. Secret names resolve through
/// [`tern_core::Secrets`] today; T4a resolves them per connection
/// (`conn/<target>/<slug>/<NAME>`) and adds OAuth tokens as another source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Inject {
    /// `<header>: <scheme> <value>`, or the bare value when `scheme` is empty.
    Header {
        header: String,
        scheme: String,
        secret: String,
    },
    /// `?<param>=<value>` appended to the URL.
    Query { param: String, secret: String },
    /// HTTP Basic from two secrets; a missing one is empty.
    Basic {
        user: Option<String>,
        password: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    #[serde(flatten)]
    pub inject: Inject,
    /// Declared hosts it is sent to; empty means every `capabilities.http` host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<String>,
}

impl Auth {
    pub fn secrets(&self) -> Vec<&str> {
        match &self.inject {
            Inject::Header { secret, .. } | Inject::Query { secret, .. } => vec![secret],
            Inject::Basic { user, password } => user
                .iter()
                .chain(password.iter())
                .map(String::as_str)
                .collect(),
        }
    }

    /// Whether requests to `host` (already allowed) carry it.
    pub fn applies_to(&self, host: &str) -> bool {
        self.hosts.is_empty() || crate::allow::host_allowed(&self.hosts, host)
    }

    /// `NOTION_TOKEN as "Authorization: Bearer …" to api.notion.com`.
    pub fn describe(&self) -> String {
        let what = match &self.inject {
            Inject::Header {
                header,
                scheme,
                secret,
            } if scheme.is_empty() => format!("{secret} as \"{header}: …\""),
            Inject::Header {
                header,
                scheme,
                secret,
            } => format!("{secret} as \"{header}: {scheme} …\""),
            Inject::Query { param, secret } => format!("{secret} as ?{param}=…"),
            Inject::Basic { user, password } => format!(
                "basic auth {}:{}",
                user.as_deref().unwrap_or("\"\""),
                password.as_deref().unwrap_or("\"\"")
            ),
        };
        if self.hosts.is_empty() {
            format!("{what} to every http host")
        } else {
            format!("{what} to {}", self.hosts.join(", "))
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub idempotent: bool,
    pub requires_approval: bool,
    /// As declared; `None` means `write` ([`ToolDef::effect`]), or M4's
    /// no-approval default for a grandfathered connector.
    pub declared_effect: Option<Effect>,
    /// Returns third-party text (mail bodies, web pages, chat messages).
    pub untrusted: bool,
    /// A `read` tool that may send POST (GraphQL, search APIs).
    pub read_via_post: bool,
    /// `money_movement` or `trade`: the host refuses the tool.
    pub forbidden: Option<&'static str>,
    pub params: serde_json::Value,
}

impl ToolDef {
    pub fn effect(&self) -> Effect {
        self.declared_effect.unwrap_or(Effect::Write)
    }

    /// Whether `ctx.http` may only send GET and HEAD for this tool.
    pub fn get_only(&self) -> bool {
        self.effect() == Effect::Read && !self.read_via_post
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnectorSpec {
    pub name: String,
    pub description: String,
    pub capabilities: Capabilities,
    pub tools: Vec<ToolDef>,
}

impl ConnectorSpec {
    /// Human-readable capability summary for approval prompts.
    /// `default_rate` is what an undeclared `rate` means.
    pub fn capability_summary(&self, default_rate: Rate) -> String {
        let c = &self.capabilities;
        let list = |v: &[String]| {
            if v.is_empty() {
                "none".to_owned()
            } else {
                v.join(", ")
            }
        };
        let tools: Vec<String> = self
            .tools
            .iter()
            .map(|t| format!("{} ({})", t.name, t.effect()))
            .collect();
        let auth: Vec<String> = c.auth.iter().map(Auth::describe).collect();
        let rate = match c.rate {
            Some(r) => r.to_string(),
            None => format!("{default_rate} (default)"),
        };
        let mut out = format!("http hosts: {}; ", list(&c.http));
        if let Some(o) = &c.oauth {
            out.push_str(&format!(
                "OAuth sign-in: {} (each account signs in separately; tern keeps the tokens); ",
                o.describe()
            ));
        }
        if !auth.is_empty() {
            out.push_str(&format!(
                "auth (tern sends it, the script never sees it): {}; ",
                auth.join("; ")
            ));
        }
        out.push_str(&format!(
            "secrets: {}; rate: {rate}; tools: {}",
            list(&c.secrets),
            tools.join(", ")
        ));
        if c.legacy_secrets() {
            out.push_str(&format!(
                ". ⚠ legacy: the script reads {} with ctx.secret and could send it anywhere it may reach; capabilities.auth keeps the value from the script",
                c.secrets.join(", ")
            ));
        }
        out
    }
}

/// Lowercase identifier usable inside a tool name: `[a-z][a-z0-9_]*`, ≤ 32.
pub fn valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    s.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn invalid(msg: impl Into<String>) -> LoadError {
    LoadError::Invalid(msg.into())
}

fn string_list(t: Option<Table>, what: &str) -> Result<Vec<String>, LoadError> {
    let Some(t) = t else {
        return Ok(Vec::new());
    };
    t.sequence_values::<String>()
        .collect::<mlua::Result<Vec<_>>>()
        .map_err(|_| invalid(format!("{what} must be a list of strings")))
}

fn check_http(entries: &[String]) -> Result<(), LoadError> {
    entries
        .iter()
        .try_for_each(|e| crate::allow::check_entry(e))
        .map_err(invalid)
}

/// The most credentials one connector may inject.
const MAX_AUTH: usize = 8;

/// `capabilities.auth`: `"NAME"` (a Bearer token), one table, or a list of
/// either.
fn parse_auth(v: Value, http: &[String], oauth: Option<&str>) -> Result<Vec<Auth>, LoadError> {
    let entries: Vec<Value> = match v {
        // OAuth with no `auth`: a Bearer token to every declared host.
        Value::Nil => {
            return Ok(oauth
                .map(|p| {
                    vec![Auth {
                        inject: Inject::Header {
                            header: "Authorization".into(),
                            scheme: "Bearer".into(),
                            secret: oauth_secret(p),
                        },
                        hosts: Vec::new(),
                    }]
                })
                .unwrap_or_default());
        }
        Value::Table(t) if t.raw_len() > 0 => t
            .sequence_values::<Value>()
            .collect::<mlua::Result<_>>()
            .map_err(|_| invalid("capabilities.auth must be a list of entries"))?,
        v @ (Value::Table(_) | Value::String(_)) => vec![v],
        _ => {
            return Err(invalid(
                "capabilities.auth must be a secret name or a table",
            ));
        }
    };
    if entries.len() > MAX_AUTH {
        return Err(invalid(format!(
            "capabilities.auth has more than {MAX_AUTH} entries"
        )));
    }
    if !entries.is_empty() && http.is_empty() {
        return Err(invalid(
            "capabilities.auth needs capabilities.http: the hosts it is sent to",
        ));
    }
    let auth = entries
        .into_iter()
        .map(|e| parse_auth_entry(e, http, oauth))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(auth)
}

fn secret_name(
    v: Option<String>,
    what: &str,
    oauth: Option<&str>,
) -> Result<Option<String>, LoadError> {
    let Some(mut name) = v else { return Ok(None) };
    // `"oauth"` or `"oauth:<provider>"`: the connection's access token.
    if name == "oauth" || is_oauth_secret(&name) {
        let Some(provider) = oauth else {
            return Err(invalid(format!(
                "capabilities.auth {what} {name:?} needs capabilities.oauth (the provider and scopes)"
            )));
        };
        if name == "oauth" {
            name = oauth_secret(provider);
        }
        if name != oauth_secret(provider) {
            return Err(invalid(format!(
                "capabilities.auth {what} {name:?}: this connector signs in with {provider:?} (capabilities.oauth)"
            )));
        }
        return Ok(Some(name));
    }
    tern_core::secrets::check_name(&name).map_err(|_| {
        invalid(format!(
            "capabilities.auth {what} {name:?} isn't a valid secret name"
        ))
    })?;
    Ok(Some(name))
}

fn parse_auth_entry(v: Value, http: &[String], oauth: Option<&str>) -> Result<Auth, LoadError> {
    let t = match v {
        Value::String(s) => {
            let secret =
                secret_name(Some(s.to_string_lossy()), "secret", oauth)?.unwrap_or_default();
            return Ok(Auth {
                inject: Inject::Header {
                    header: "Authorization".into(),
                    scheme: "Bearer".into(),
                    secret,
                },
                hosts: Vec::new(),
            });
        }
        Value::Table(t) => t,
        _ => {
            return Err(invalid(
                "each capabilities.auth entry must be a secret name or a table",
            ));
        }
    };
    const KEYS: &[&str] = &["secret", "header", "scheme", "query", "basic", "hosts"];
    for pair in t.pairs::<Value, Value>() {
        let (k, _) = pair.map_err(|_| invalid("capabilities.auth is unreadable"))?;
        let k = k.to_string().unwrap_or_default();
        if !KEYS.contains(&k.as_str()) {
            return Err(invalid(format!(
                "capabilities.auth: unknown key {k:?} (expected secret, header, scheme, query, basic or hosts)"
            )));
        }
    }
    let text = |k: &str| -> Result<Option<String>, LoadError> {
        t.raw_get::<Option<String>>(k)
            .map_err(|_| invalid(format!("capabilities.auth.{k} must be a string")))
    };
    let secret = secret_name(text("secret")?, "secret", oauth)?;
    let (header, scheme, query) = (text("header")?, text("scheme")?, text("query")?);
    let basic: Option<Table> = t.raw_get("basic").map_err(|_| {
        invalid("capabilities.auth.basic must be { user = \"SECRET\", password = \"SECRET\" }")
    })?;
    let inject = if let Some(b) = basic {
        if secret.is_some() || header.is_some() || scheme.is_some() || query.is_some() {
            return Err(invalid(
                "capabilities.auth: `basic` can't be combined with secret, header, scheme or query",
            ));
        }
        let field = |k: &str| -> Result<Option<String>, LoadError> {
            let v = b.raw_get::<Option<String>>(k).map_err(|_| {
                invalid(format!("capabilities.auth.basic.{k} must be a secret name"))
            })?;
            let name = secret_name(v, &format!("basic.{k}"), oauth)?;
            if name.as_deref().is_some_and(is_oauth_secret) {
                return Err(invalid(
                    "capabilities.auth.basic can't carry an OAuth token",
                ));
            }
            Ok(name)
        };
        let (user, password) = (field("user")?, field("password")?);
        if user.is_none() && password.is_none() {
            return Err(invalid(
                "capabilities.auth.basic needs a user or password secret",
            ));
        }
        Inject::Basic { user, password }
    } else {
        let secret =
            secret.ok_or_else(|| invalid("capabilities.auth needs `secret`, the secret's name"))?;
        if let Some(param) = query {
            if header.is_some() || scheme.is_some() {
                return Err(invalid(
                    "capabilities.auth: `query` can't be combined with header or scheme",
                ));
            }
            let ok = !param.is_empty()
                && param
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
            if !ok {
                return Err(invalid(format!(
                    "capabilities.auth.query {param:?} must be a plain parameter name"
                )));
            }
            Inject::Query { param, secret }
        } else {
            let header = header.unwrap_or_else(|| "Authorization".into());
            let name =
                reqwest::header::HeaderName::from_bytes(header.as_bytes()).map_err(|_| {
                    invalid(format!(
                        "capabilities.auth.header {header:?} isn't a valid header name"
                    ))
                })?;
            if matches!(
                name.as_str(),
                "host" | "content-length" | "transfer-encoding" | "connection"
            ) {
                return Err(invalid(format!(
                    "capabilities.auth.header can't be {header:?}"
                )));
            }
            let scheme = scheme.unwrap_or_else(|| {
                if name == reqwest::header::AUTHORIZATION {
                    "Bearer".into()
                } else {
                    String::new()
                }
            });
            if !scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            {
                return Err(invalid(format!(
                    "capabilities.auth.scheme {scheme:?} must be one word, like \"Bearer\" or \"Token\""
                )));
            }
            Inject::Header {
                header,
                scheme,
                secret,
            }
        }
    };
    let hosts = string_list(
        t.raw_get("hosts")
            .map_err(|_| invalid("capabilities.auth.hosts must be a list"))?,
        "capabilities.auth.hosts",
    )?;
    for h in &hosts {
        let covered = http
            .iter()
            .any(|e| crate::allow::entry_host(e).eq_ignore_ascii_case(h))
            || (!h.starts_with("*.") && crate::allow::host_allowed(http, h));
        if !covered {
            return Err(invalid(format!(
                "capabilities.auth.hosts entry {h:?} must be one of the hosts in capabilities.http"
            )));
        }
    }
    Ok(Auth { inject, hosts })
}

/// `capabilities.oauth = { google = { scopes = { "gmail.readonly" } } }`:
/// one provider (a built-in profile or a discovered one) and its scopes.
fn parse_oauth(v: Option<Table>, http: &[String]) -> Result<Option<OAuthCap>, LoadError> {
    let Some(t) = v else { return Ok(None) };
    let mut found = None;
    for pair in t.pairs::<String, Value>() {
        let (provider, conf) = pair
            .map_err(|_| invalid("capabilities.oauth must be { provider = { scopes = { … } } }"))?;
        if found.is_some() {
            return Err(invalid("capabilities.oauth names more than one provider"));
        }
        if !valid_ident(&provider) {
            return Err(invalid(format!(
                "capabilities.oauth provider {provider:?} must match [a-z][a-z0-9_]*"
            )));
        }
        let scopes = match conf {
            Value::Table(c) => {
                for pair in c.pairs::<String, Value>() {
                    let (k, _) = pair.map_err(|_| invalid("capabilities.oauth is unreadable"))?;
                    if k != "scopes" {
                        return Err(invalid(format!(
                            "capabilities.oauth.{provider}: unknown key {k:?} (expected scopes)"
                        )));
                    }
                }
                string_list(
                    c.raw_get("scopes")
                        .map_err(|_| invalid("capabilities.oauth scopes must be a list"))?,
                    "capabilities.oauth scopes",
                )?
            }
            Value::Boolean(true) => Vec::new(),
            _ => {
                return Err(invalid(
                    "capabilities.oauth must be { provider = { scopes = { … } } }",
                ));
            }
        };
        if scopes.len() > 50
            || scopes
                .iter()
                .any(|s| s.is_empty() || s.len() > 200 || s.contains(char::is_whitespace))
        {
            return Err(invalid(
                "capabilities.oauth scopes must be up to 50 words without spaces",
            ));
        }
        found = Some(OAuthCap { provider, scopes });
    }
    if found.is_some() && http.is_empty() {
        return Err(invalid(
            "capabilities.oauth needs capabilities.http: the hosts the token is sent to",
        ));
    }
    Ok(found)
}

pub fn parse(lua: &Lua, t: &Table) -> Result<(ConnectorSpec, Vec<(String, Function)>), LoadError> {
    let get_str = |key: &str| -> Result<String, LoadError> {
        match t.raw_get::<Value>(key) {
            Ok(Value::String(s)) => Ok(s.to_string_lossy()),
            _ => Err(invalid(format!("`{key}` must be a string"))),
        }
    };
    let name = get_str("name")?;
    if !valid_ident(&name) {
        return Err(invalid(format!(
            "name {name:?} must match [a-z][a-z0-9_]* (max 32 chars)"
        )));
    }
    let description = get_str("description")?;

    let caps: Option<Table> = t
        .raw_get("capabilities")
        .map_err(|_| invalid("`capabilities` must be a table"))?;
    let capabilities = match caps {
        None => Capabilities::default(),
        Some(c) => {
            let http = string_list(
                c.raw_get("http")
                    .map_err(|_| invalid("capabilities.http must be a list"))?,
                "capabilities.http",
            )?;
            check_http(&http)?;
            let oauth = parse_oauth(
                c.raw_get("oauth")
                    .map_err(|_| invalid("capabilities.oauth must be a table"))?,
                &http,
            )?;
            let auth = parse_auth(
                c.raw_get("auth")
                    .map_err(|_| invalid("capabilities.auth is unreadable"))?,
                &http,
                oauth.as_ref().map(|o| o.provider.as_str()),
            )?;
            let rate = match c.raw_get::<Value>("rate") {
                Ok(Value::Nil) => None,
                Ok(Value::String(r)) => Some(
                    r.to_string_lossy()
                        .parse::<Rate>()
                        .map_err(|e| invalid(format!("capabilities.{e}")))?,
                ),
                _ => {
                    return Err(invalid(
                        "capabilities.rate must be a string like \"60/min\"",
                    ));
                }
            };
            Capabilities {
                http,
                secrets: string_list(
                    c.raw_get("secrets")
                        .map_err(|_| invalid("capabilities.secrets must be a list"))?,
                    "capabilities.secrets",
                )?,
                auth,
                rate,
                oauth,
            }
        }
    };
    if let Some(both) = capabilities
        .auth_secrets()
        .into_iter()
        .find(|s| capabilities.secrets.contains(s))
    {
        return Err(invalid(format!(
            "{both} is in both capabilities.auth and capabilities.secrets; remove it from secrets so the script never sees it"
        )));
    }

    let tools: Table = t
        .raw_get::<Option<Table>>("tools")
        .ok()
        .flatten()
        .ok_or_else(|| invalid("`tools` must be a table of tool definitions"))?;
    let mut defs = Vec::new();
    let mut runs = Vec::new();
    for pair in tools.pairs::<String, Table>() {
        let (tool, def) =
            pair.map_err(|_| invalid("each entry in `tools` must be `name = { ... }`"))?;
        if !valid_ident(&tool) {
            return Err(invalid(format!(
                "tool name {tool:?} must match [a-z][a-z0-9_]* (max 32 chars)"
            )));
        }
        let ctx = |m: &str| invalid(format!("tools.{tool}: {m}"));
        let description: String = def
            .raw_get::<Option<String>>("description")
            .ok()
            .flatten()
            .ok_or_else(|| ctx("`description` must be a string"))?;
        let run: Function = def
            .raw_get::<Option<Function>>("run")
            .ok()
            .flatten()
            .ok_or_else(|| ctx("`run` must be a function"))?;
        let flag = |k: &str| -> Result<bool, LoadError> {
            match def.raw_get::<Value>(k) {
                Ok(Value::Nil) => Ok(false),
                Ok(Value::Boolean(b)) => Ok(b),
                _ => Err(ctx(&format!("`{k}` must be a boolean"))),
            }
        };
        let params_value: Value = def
            .raw_get("params")
            .map_err(|_| ctx("`params` is unreadable"))?;
        let params: serde_json::Value = match params_value {
            Value::Nil => serde_json::json!({ "type": "object", "properties": {} }),
            v @ Value::Table(_) => lua
                .from_value_with(
                    v,
                    mlua::serde::DeserializeOptions::new().encode_empty_tables_as_array(false),
                )
                .map_err(|e| ctx(&format!("`params` is not valid JSON: {e}")))?,
            _ => return Err(ctx("`params` must be a JSON Schema table")),
        };
        check_schema(&params).map_err(|m| ctx(&format!("params: {m}")))?;
        let declared_effect = match def.raw_get::<Value>("effect") {
            Ok(Value::Nil) => None,
            Ok(Value::String(e)) => {
                Some(Effect::parse(&e.to_string_lossy()).ok_or_else(|| {
                    ctx("`effect` must be \"read\", \"write\" or \"destructive\"")
                })?)
            }
            _ => {
                return Err(ctx(
                    "`effect` must be \"read\", \"write\" or \"destructive\"",
                ));
            }
        };
        let read_via_post = flag("read_via_post")?;
        if read_via_post && declared_effect != Some(Effect::Read) {
            return Err(ctx("`read_via_post` only applies to `effect = \"read\"`"));
        }
        let forbidden = if flag("money_movement")? {
            Some("money_movement")
        } else if flag("trade")? {
            Some("trade")
        } else {
            None
        };
        defs.push(ToolDef {
            name: tool.clone(),
            description,
            idempotent: flag("idempotent")?,
            requires_approval: flag("requires_approval")?,
            declared_effect,
            untrusted: flag("untrusted")?,
            read_via_post,
            forbidden,
            params,
        });
        runs.push((tool, run));
    }
    if defs.is_empty() {
        return Err(invalid("`tools` is empty"));
    }
    defs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((
        ConnectorSpec {
            name,
            description,
            capabilities,
            tools: defs,
        },
        runs,
    ))
}

/// The subset of JSON Schema checks that catch real mistakes: an object
/// schema whose properties are schemas and whose `required` names exist.
pub fn check_schema(s: &serde_json::Value) -> Result<(), String> {
    let obj = s.as_object().ok_or("must be an object")?;
    if obj.get("type").and_then(|t| t.as_str()) != Some("object") {
        return Err("top-level `type` must be \"object\"".into());
    }
    let props = match obj.get("properties") {
        None => None,
        Some(serde_json::Value::Object(p)) => Some(p),
        Some(_) => return Err("`properties` must be a table of schemas".into()),
    };
    if let Some(p) = props {
        for (k, v) in p {
            let Some(v) = v.as_object() else {
                return Err(format!("properties.{k} must be a schema table"));
            };
            if let Some(t) = v.get("type")
                && !t.is_string()
                && !t.is_array()
            {
                return Err(format!("properties.{k}.type must be a string"));
            }
        }
    }
    if let Some(req) = obj.get("required") {
        let req = req.as_array().ok_or("`required` must be a list")?;
        for r in req {
            let name = r.as_str().ok_or("`required` entries must be strings")?;
            if !props.is_some_and(|p| p.contains_key(name)) {
                return Err(format!("required property {name:?} is not in `properties`"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schema_checks() {
        assert!(check_schema(&json!({"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]})).is_ok());
        assert!(check_schema(&json!({"type": "string"})).is_err());
        assert!(
            check_schema(&json!({"type": "object", "required": ["a"]}))
                .unwrap_err()
                .contains("\"a\"")
        );
        assert!(check_schema(&json!({"type": "object", "properties": {"a": 1}})).is_err());
    }

    #[test]
    fn idents() {
        assert!(valid_ident("linear") && valid_ident("create_issue2"));
        assert!(
            !valid_ident("Linear") && !valid_ident("2x") && !valid_ident("a-b") && !valid_ident("")
        );
    }
}
