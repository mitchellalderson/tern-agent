//! One sandboxed Luau VM per connector, and the `ctx` API connectors call.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use mlua::{Function, Lua, LuaOptions, LuaSerdeExt, StdLib, Table, Value, VmState};
use tern_core::Secrets;
use tern_store::Store;

use crate::spec::{Capabilities, ConnectorSpec};

/// Per-call wall-clock limit (spec §10.1).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// Per-VM memory limit (spec §10.1).
pub const DEFAULT_MEMORY: usize = 64 * 1024 * 1024;
/// Largest HTTP response body a connector can read.
const MAX_HTTP_BODY: usize = 5 * 1024 * 1024;

/// How users provide secrets; shown when one is missing so the model
/// relays the real mechanism instead of inventing one (like a `.env` file).
pub const SECRETS_HELP: &str = "Call secret_request with names [\"{key}\"] and why it's needed: the user \
enters it in a private prompt you never see, and it works straight away. Never ask the user to paste a \
secret into the chat. (Users can also set secrets as environment variables or in ~/.tern/secrets.toml; \
tern does not read .env files.)";

/// Globals removed before sandboxing: code loading and environment access.
const REMOVED_GLOBALS: &[&str] = &[
    "require",
    "loadstring",
    "load",
    "dofile",
    "loadfile",
    "getfenv",
    "setfenv",
];

const TO_LUA: mlua::serde::SerializeOptions = mlua::serde::SerializeOptions::new()
    .serialize_none_to_null(false)
    .serialize_unit_to_null(false);
const FROM_LUA: mlua::serde::DeserializeOptions = mlua::serde::DeserializeOptions::new()
    .deny_unsupported_types(false)
    .encode_empty_tables_as_array(false);

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{0}")]
    Lua(String),
    #[error("invalid connector: {0}")]
    Invalid(String),
}

/// Shared resources every connector VM can reach, through its capabilities.
#[derive(Clone)]
pub struct Host {
    pub store: Store,
    pub secrets: Arc<Secrets>,
    pub timeout: Duration,
    pub memory: usize,
}

impl Host {
    pub fn new(store: Store, secrets: Secrets) -> Result<Self, reqwest::Error> {
        Ok(Self {
            store,
            secrets: Arc::new(secrets),
            timeout: DEFAULT_TIMEOUT,
            memory: DEFAULT_MEMORY,
        })
    }
}

/// A loaded connector: its validated spec and the VM holding its functions.
pub struct Vm {
    pub spec: ConnectorSpec,
    lua: Lua,
    runs: Vec<(String, Function)>,
    deadline: Arc<Mutex<Option<Instant>>>,
    host: Host,
    /// This connector's HTTP client: redirects are checked against its allowlist.
    http: reqwest::Client,
    /// One call at a time per VM (the deadline is per VM).
    busy: tokio::sync::Mutex<()>,
    /// Keeps M4's approval default for tools that declare no effect (T0
    /// grandfathering; see `crate::grandfathered`).
    pub grandfathered: bool,
}

impl Vm {
    /// Load and validate connector `source`. `chunk` names it in errors.
    pub fn load(host: &Host, chunk: &str, source: &str) -> Result<Self, LoadError> {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default()).map_err(lua_err)?;
        lua.set_memory_limit(host.memory).map_err(lua_err)?;
        let deadline: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let d = deadline.clone();
        lua.set_interrupt(move |_| {
            let expired = d
                .lock()
                .ok()
                .and_then(|g| *g)
                .is_some_and(|t| Instant::now() >= t);
            if expired {
                return Err(mlua::Error::runtime(
                    "timed out: connector exceeded its time limit",
                ));
            }
            Ok(VmState::Continue)
        });
        install_globals(&lua, chunk, host.secrets.clone()).map_err(lua_err)?;
        lua.sandbox(true).map_err(lua_err)?;

        set_deadline(&deadline, Some(host.timeout));
        let loaded = lua
            .load(source)
            .set_name(format!("={chunk}"))
            .eval::<Value>();
        set_deadline(&deadline, None);
        let Value::Table(table) = loaded.map_err(lua_err)? else {
            return Err(LoadError::Invalid("the file must `return` a table".into()));
        };
        let (spec, runs) = crate::spec::parse(&lua, &table)?;
        let http = connector_client(&spec.capabilities)?;
        Ok(Self {
            spec,
            lua,
            runs,
            deadline,
            host: host.clone(),
            http,
            busy: tokio::sync::Mutex::new(()),
            grandfathered: false,
        })
    }

    /// Run tool `name` with JSON `args`. Errors (including Lua errors and
    /// timeouts) come back as `Err(message)`, never panics.
    pub async fn call(&self, name: &str, args: &serde_json::Value) -> Result<String, String> {
        let Some((_, run)) = self.runs.iter().find(|(n, _)| n == name) else {
            return Err(format!("connector {} has no tool {name:?}", self.spec.name));
        };
        let get_only = self
            .spec
            .tools
            .iter()
            .find(|t| t.name == name)
            .is_some_and(|t| t.get_only())
            .then_some(name);
        let _guard = self.busy.lock().await;
        let ctx = make_ctx(&self.lua, &self.host, &self.http, &self.spec, get_only)
            .map_err(|e| format!("building ctx: {e}"))?;
        let lua_args = self
            .lua
            .to_value_with(args, TO_LUA)
            .map_err(|e| format!("converting arguments: {e}"))?;

        set_deadline(&self.deadline, Some(self.host.timeout));
        let fut = run.call_async::<Value>((ctx, lua_args));
        let result = tokio::time::timeout(self.host.timeout, fut).await;
        set_deadline(&self.deadline, None);

        let value = match result {
            Err(_) => {
                return Err(format!(
                    "timed out after {}s",
                    self.host.timeout.as_secs_f64()
                ));
            }
            Ok(Err(e)) => return Err(clean_error(&e)),
            Ok(Ok(v)) => v,
        };
        match value {
            Value::Nil => Ok("ok".into()),
            Value::String(s) => Ok(s.to_string_lossy()),
            other => {
                let json: serde_json::Value = self
                    .lua
                    .from_value_with(other, FROM_LUA)
                    .map_err(|e| format!("converting result: {e}"))?;
                serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
            }
        }
    }
}

fn set_deadline(d: &Mutex<Option<Instant>>, after: Option<Duration>) {
    if let Ok(mut g) = d.lock() {
        *g = after.map(|a| Instant::now() + a);
    }
}

fn lua_err(e: mlua::Error) -> LoadError {
    LoadError::Lua(clean_error(&e))
}

/// The useful part of an mlua error: the innermost message, no traceback.
pub fn clean_error(e: &mlua::Error) -> String {
    let mut cur = e;
    loop {
        match cur {
            mlua::Error::CallbackError { cause, .. } => cur = cause,
            mlua::Error::WithContext { cause, .. } => cur = cause,
            mlua::Error::RuntimeError(m) => return m.lines().next().unwrap_or(m).to_owned(),
            mlua::Error::SyntaxError { message, .. } => {
                return format!(
                    "syntax error: {}",
                    message.lines().next().unwrap_or(message)
                );
            }
            mlua::Error::MemoryError(m) => return format!("out of memory: {m}"),
            other => {
                return other
                    .to_string()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
            }
        }
    }
}

fn install_globals(lua: &Lua, chunk: &str, secrets: Arc<Secrets>) -> mlua::Result<()> {
    let g = lua.globals();
    for name in REMOVED_GLOBALS {
        g.raw_set(*name, Value::Nil)?;
    }
    // stdout belongs to the terminal UI; route print to the log.
    let chunk_name = chunk.to_owned();
    g.raw_set(
        "print",
        lua.create_function(move |_, args: mlua::Variadic<Value>| {
            let parts: Vec<String> = args
                .iter()
                .map(|v| v.to_string().unwrap_or_default())
                .collect();
            tracing::info!(connector = %chunk_name, "{}", secrets.redact(&parts.join("\t")));
            Ok(())
        })?,
    )?;
    let json = lua.create_table()?;
    json.raw_set(
        "encode",
        lua.create_function(|lua, v: Value| {
            let j: serde_json::Value = lua.from_value_with(v, FROM_LUA)?;
            serde_json::to_string(&j).map_err(mlua::Error::external)
        })?,
    )?;
    json.raw_set(
        "decode",
        lua.create_function(|lua, s: String| {
            let j: serde_json::Value = serde_json::from_str(&s).map_err(mlua::Error::external)?;
            lua.to_value_with(&j, TO_LUA)
        })?,
    )?;
    g.raw_set("json", json)?;
    Ok(())
}

/// Build the `ctx` table for one call, bound to this connector's capabilities.
fn make_ctx(
    lua: &Lua,
    host: &Host,
    client: &reqwest::Client,
    spec: &ConnectorSpec,
    get_only: Option<&str>,
) -> mlua::Result<Table> {
    let ctx = lua.create_table()?;
    let caps = Arc::new(spec.capabilities.clone());
    let get_only: Option<Arc<str>> = get_only.map(Arc::from);
    let name = spec.name.clone();

    let http = lua.create_table()?;
    {
        let (caps, client, get_only) = (caps.clone(), client.clone(), get_only.clone());
        http.raw_set(
            "request",
            lua.create_async_function(move |lua, opts: Table| {
                let (caps, client, get_only) = (caps.clone(), client.clone(), get_only.clone());
                async move { http_request(&lua, &caps, &client, get_only.as_deref(), opts).await }
            })?,
        )?;
    }
    for method in ["get", "post", "put", "patch", "delete"] {
        let (caps, client, get_only) = (caps.clone(), client.clone(), get_only.clone());
        http.raw_set(
            method,
            lua.create_async_function(move |lua, (url, opts): (String, Option<Table>)| {
                let (caps, client, get_only) = (caps.clone(), client.clone(), get_only.clone());
                async move {
                    let opts = match opts {
                        Some(t) => t,
                        None => lua.create_table()?,
                    };
                    opts.raw_set("url", url)?;
                    opts.raw_set("method", method.to_ascii_uppercase())?;
                    http_request(&lua, &caps, &client, get_only.as_deref(), opts).await
                }
            })?,
        )?;
    }
    ctx.raw_set("http", http)?;

    {
        let (caps, secrets, name) = (caps.clone(), host.secrets.clone(), name.clone());
        ctx.raw_set(
            "secret",
            lua.create_function(move |_, key: String| {
                if !caps.secrets.contains(&key) {
                    return Err(mlua::Error::runtime(format!(
                        "secret {key:?} is not declared in {name}'s capabilities.secrets"
                    )));
                }
                secrets.get(&key).ok_or_else(|| {
                    mlua::Error::runtime(format!(
                        "secret {key:?} is not set. {}",
                        SECRETS_HELP.replace("{key}", &key)
                    ))
                })
            })?,
        )?;
    }

    let kv = lua.create_table()?;
    let ns = format!("lua:{name}");
    {
        let (store, ns) = (host.store.clone(), ns.clone());
        kv.raw_set(
            "get",
            lua.create_function(move |lua, key: String| {
                match store.kv_get(&ns, &key).map_err(mlua::Error::external)? {
                    None => Ok(Value::Nil),
                    Some(raw) => {
                        let j: serde_json::Value =
                            serde_json::from_str(&raw).map_err(mlua::Error::external)?;
                        lua.to_value_with(&j, TO_LUA)
                    }
                }
            })?,
        )?;
    }
    {
        let store = host.store.clone();
        kv.raw_set(
            "set",
            lua.create_function(move |lua, (key, value): (String, Value)| {
                if value.is_nil() {
                    return store.kv_delete(&ns, &key).map_err(mlua::Error::external);
                }
                let j: serde_json::Value = lua.from_value_with(value, FROM_LUA)?;
                store
                    .kv_set(&ns, &key, &j.to_string())
                    .map_err(mlua::Error::external)
            })?,
        )?;
    }
    ctx.raw_set("kv", kv)?;

    let (log_name, log_secrets) = (name.clone(), host.secrets.clone());
    ctx.raw_set(
        "log",
        lua.create_function(move |_, (level, msg): (String, String)| {
            let msg = log_secrets.redact(&msg);
            match level.as_str() {
                "error" => tracing::error!(connector = %log_name, "{msg}"),
                "warn" => tracing::warn!(connector = %log_name, "{msg}"),
                "debug" => tracing::debug!(connector = %log_name, "{msg}"),
                _ => tracing::info!(connector = %log_name, "{msg}"),
            }
            Ok(())
        })?,
    )?;
    ctx.raw_set(
        "now",
        lua.create_function(|_, ()| Ok(tern_store::now_ms()))?,
    )?;
    Ok(ctx)
}

/// An error and its causes on one line (reqwest hides the reason a redirect
/// was refused in the source chain).
fn error_chain(e: &reqwest::Error) -> String {
    let mut msg = e.to_string();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(c) = cur {
        msg = format!("{msg}: {c}");
        cur = c.source();
    }
    msg
}

/// An HTTP client that follows a redirect only if its target is also on the
/// connector's allowlist (reqwest's default would follow anywhere, carrying
/// custom headers like `X-Api-Key` along).
fn connector_client(caps: &Capabilities) -> Result<reqwest::Client, LoadError> {
    let allowed = caps.http.clone();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        let host = attempt.url().host_str().unwrap_or_default().to_owned();
        if attempt.previous().len() >= 5 {
            attempt.error("too many redirects")
        } else if !matches!(attempt.url().scheme(), "http" | "https")
            || !host_allowed(&allowed, &host)
        {
            attempt.error(format!(
                "redirect to {host:?}, which is not in this connector's capabilities.http"
            ))
        } else {
            attempt.follow()
        }
    });
    tern_core::net::http_client()
        .redirect(policy)
        .build()
        .map_err(|e| LoadError::Lua(format!("http client: {e}")))
}

/// `true` if `host` is allowed by an entry like `api.example.com` or `*.example.com`.
pub fn host_allowed(allowed: &[String], host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    allowed.iter().any(|a| {
        let a = a.to_ascii_lowercase();
        match a.strip_prefix("*.") {
            Some(suffix) => host.ends_with(&format!(".{suffix}")),
            None => a == host,
        }
    })
}

async fn http_request(
    lua: &Lua,
    caps: &Capabilities,
    client: &reqwest::Client,
    get_only: Option<&str>,
    opts: Table,
) -> mlua::Result<Table> {
    let url: String = opts
        .raw_get::<Option<String>>("url")?
        .ok_or_else(|| mlua::Error::runtime("http.request: `url` is required"))?;
    let parsed = reqwest::Url::parse(&url)
        .map_err(|e| mlua::Error::runtime(format!("http: invalid url {url:?}: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(mlua::Error::runtime(format!(
            "http: unsupported scheme {:?}",
            parsed.scheme()
        )));
    }
    let host = parsed.host_str().unwrap_or_default().to_owned();
    if !host_allowed(&caps.http, &host) {
        let allowed = if caps.http.is_empty() {
            "none".to_owned()
        } else {
            caps.http.join(", ")
        };
        return Err(mlua::Error::runtime(format!(
            "http: host {host:?} is not in this connector's capabilities.http (allowed: {allowed})"
        )));
    }
    let method: String = opts
        .raw_get::<Option<String>>("method")?
        .unwrap_or_else(|| "GET".into());
    let method = reqwest::Method::from_bytes(method.to_ascii_uppercase().as_bytes())
        .map_err(|_| mlua::Error::runtime(format!("http: invalid method {method:?}")))?;
    // Checked before anything is sent: a `read` tool can't write by POST.
    if let Some(tool) = get_only
        && !matches!(method, reqwest::Method::GET | reqwest::Method::HEAD)
    {
        return Err(mlua::Error::runtime(format!(
            "http: tools.{tool} declares effect = \"read\" but sent {method}; declare effect = \"write\", or set read_via_post = true if it only reads (GraphQL, search APIs)"
        )));
    }
    let mut req = client.request(method, parsed);
    if let Some(headers) = opts.raw_get::<Option<Table>>("headers")? {
        for pair in headers.pairs::<String, String>() {
            let (k, v) = pair?;
            req = req.header(k, v);
        }
    }
    let json_body: Value = opts.raw_get("json")?;
    if !json_body.is_nil() {
        let j: serde_json::Value = lua.from_value_with(json_body, FROM_LUA)?;
        req = req.json(&j);
    } else if let Some(body) = opts.raw_get::<Option<String>>("body")? {
        req = req.body(body);
    }
    if let Some(secs) = opts.raw_get::<Option<f64>>("timeout")? {
        req = req.timeout(Duration::from_secs_f64(secs.clamp(0.1, 300.0)));
    }

    let resp = req.send().await.map_err(|e| {
        mlua::Error::runtime(format!(
            "http: request to {host} failed: {}",
            error_chain(&e)
        ))
    })?;
    let status = resp.status().as_u16();
    let headers = lua.create_table()?;
    for (k, v) in resp.headers() {
        if let Ok(v) = v.to_str() {
            headers.raw_set(k.as_str(), v)?;
        }
    }
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| mlua::Error::runtime(format!("http: reading body: {e}")))?;
        if body.len() + chunk.len() > MAX_HTTP_BODY {
            return Err(mlua::Error::runtime(format!(
                "http: response body exceeds {MAX_HTTP_BODY} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&body).into_owned();
    let out = lua.create_table()?;
    out.raw_set("status", status)?;
    out.raw_set("ok", (200..300).contains(&status))?;
    out.raw_set("headers", headers)?;
    if let Ok(j) = serde_json::from_str::<serde_json::Value>(&text) {
        out.raw_set("json", lua.to_value_with(&j, TO_LUA)?)?;
    }
    out.raw_set("body", text)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allowlist() {
        let allowed = vec!["api.linear.app".to_owned(), "*.example.com".to_owned()];
        assert!(host_allowed(&allowed, "api.linear.app"));
        assert!(host_allowed(&allowed, "API.Linear.app"));
        assert!(host_allowed(&allowed, "a.b.example.com"));
        assert!(!host_allowed(&allowed, "example.com"));
        assert!(!host_allowed(&allowed, "api.linear.app.evil.com"));
        assert!(!host_allowed(&allowed, "evillinear.app"));
        assert!(!host_allowed(&[], "anything"));
    }
}
