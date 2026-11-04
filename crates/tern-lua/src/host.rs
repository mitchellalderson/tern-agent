//! One sandboxed Luau VM per connector, and the `ctx` API connectors call.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use mlua::{Function, Lua, LuaOptions, LuaSerdeExt, StdLib, Table, Value, VmState};
use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use tern_core::Secrets;
use tern_core::config::Rate;
use tern_store::Store;

use tern_auth::{Connections, Resolved};

use crate::allow::{entry_host, host_allowed, path_allowed};
use crate::limit::Limiter;
use crate::spec::{Auth, Capabilities, ConnectorSpec, Inject, is_oauth_secret};

/// Per-call wall-clock limit (spec §10.1).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
/// Per-VM memory limit (spec §10.1).
pub const DEFAULT_MEMORY: usize = 64 * 1024 * 1024;
/// Largest HTTP response body a connector can read.
const MAX_HTTP_BODY: usize = 5 * 1024 * 1024;
/// Redirects `ctx.http` follows, each checked like the first request.
const MAX_REDIRECTS: usize = 5;
/// Requests per connector when it declares no `rate` (`[connectors] rate`).
pub const DEFAULT_RATE: Rate = Rate::per_minute(120);

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
    /// For connectors that declare no `rate`.
    pub default_rate: Rate,
    /// Request budgets by connector name, kept across reloads.
    pub limiter: Arc<Limiter>,
    /// Accounts (tooling spec §6.8). `None` (tests, tools that don't run
    /// connectors): every call uses plain secret names.
    pub connections: Option<Arc<Connections>>,
}

impl Host {
    pub fn new(store: Store, secrets: Secrets) -> Result<Self, reqwest::Error> {
        Ok(Self {
            store,
            secrets: Arc::new(secrets),
            timeout: DEFAULT_TIMEOUT,
            memory: DEFAULT_MEMORY,
            default_rate: DEFAULT_RATE,
            limiter: Arc::default(),
            connections: None,
        })
    }
}

/// A loaded connector: its validated spec and the VM holding its functions.
pub struct Vm {
    pub spec: ConnectorSpec,
    lua: Lua,
    runs: Vec<(String, Function)>,
    deadline: Arc<Mutex<Option<Instant>>>,
    pub(crate) host: Host,
    /// This connector's HTTP client. It follows no redirects: `ctx.http`
    /// follows them itself, checking each hop.
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
        let http = tern_core::net::http_client()
            .build()
            .map_err(|e| LoadError::Lua(format!("http client: {e}")))?;
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

    /// The approval prompt's summary, with this host's default rate.
    pub fn capability_summary(&self) -> String {
        self.spec.capability_summary(self.host.default_rate)
    }

    /// Run tool `name` with JSON `args`. Errors (including Lua errors and
    /// timeouts) come back as `Err(message)`, never panics.
    pub async fn call(&self, name: &str, args: &serde_json::Value) -> Result<String, String> {
        self.call_as(name, args, None).await
    }

    /// [`Vm::call`] under an account (`None`: the implicit default).
    pub async fn call_as(
        &self,
        name: &str,
        args: &serde_json::Value,
        conn: Option<Resolved>,
    ) -> Result<String, String> {
        let Some((_, run)) = self.runs.iter().find(|(n, _)| n == name) else {
            return Err(format!("connector {} has no tool {name:?}", self.spec.name));
        };
        let get_only = self
            .spec
            .tools
            .iter()
            .find(|t| t.name == name)
            .is_some_and(|t| t.get_only())
            .then(|| name.to_owned());
        let _guard = self.busy.lock().await;
        let net = Arc::new(Net {
            connector: self.spec.name.clone(),
            caps: self.spec.capabilities.clone(),
            client: self.http.clone(),
            get_only,
            secrets: self.host.secrets.clone(),
            limiter: self.host.limiter.clone(),
            rate: self
                .spec
                .capabilities
                .rate
                .unwrap_or(self.host.default_rate),
            conn,
            connections: self.host.connections.clone(),
        });
        let ctx = make_ctx(&self.lua, &self.host, net, &self.spec)
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

/// What `ctx.http` works with for one call.
struct Net {
    connector: String,
    caps: Capabilities,
    client: reqwest::Client,
    /// The tool, when it may only send GET and HEAD.
    get_only: Option<String>,
    secrets: Arc<Secrets>,
    limiter: Arc<Limiter>,
    rate: Rate,
    /// The account this call runs under (`None`: the implicit default).
    conn: Option<Resolved>,
    connections: Option<Arc<Connections>>,
}

impl Net {
    /// A credential's value, for the host to send: the call's account's
    /// secret (`conn/<target>/<slug>/<NAME>`, the plain name for the
    /// default), or its OAuth access token. `rejected` is a token the
    /// service just refused, to refresh past.
    async fn secret(&self, name: &str, rejected: Option<&str>) -> mlua::Result<String> {
        if is_oauth_secret(name) {
            let (Some(conns), Some(conn)) = (&self.connections, &self.conn) else {
                return Err(mlua::Error::runtime(format!(
                    "{} signs in with OAuth and has no connected account yet: call connection_add so the user can sign in",
                    self.connector
                )));
            };
            return conns
                .access_token(conn, rejected)
                .await
                .map_err(mlua::Error::runtime);
        }
        let scoped = Connections::secret_name(self.conn.as_ref(), name);
        self.secrets.get(&scoped).ok_or_else(|| {
            mlua::Error::runtime(match &self.conn {
                Some(c) if c.conn.slug != tern_auth::DEFAULT_SLUG => format!(
                    "secret {name:?} is not set for {}: call connection_add with target {:?}, connection {:?} and method \"key\" so the user can enter it",
                    c.conn.title(),
                    c.conn.target,
                    c.conn.slug
                ),
                _ => format!(
                    "secret {name:?} is not set. {}",
                    SECRETS_HELP.replace("{key}", name)
                ),
            })
        })
    }
}

/// Build the `ctx` table for one call, bound to this connector's capabilities.
fn make_ctx(lua: &Lua, host: &Host, net: Arc<Net>, spec: &ConnectorSpec) -> mlua::Result<Table> {
    let ctx = lua.create_table()?;
    let caps = Arc::new(spec.capabilities.clone());
    let name = spec.name.clone();

    let http = lua.create_table()?;
    {
        let net = net.clone();
        http.raw_set(
            "request",
            lua.create_async_function(move |lua, opts: Table| {
                let net = net.clone();
                async move { http_request(&lua, &net, opts).await }
            })?,
        )?;
    }
    for method in ["get", "post", "put", "patch", "delete"] {
        let net = net.clone();
        http.raw_set(
            method,
            lua.create_async_function(move |lua, (url, opts): (String, Option<Table>)| {
                let net = net.clone();
                async move {
                    let opts = match opts {
                        Some(t) => t,
                        None => lua.create_table()?,
                    };
                    opts.raw_set("url", url)?;
                    opts.raw_set("method", method.to_ascii_uppercase())?;
                    http_request(&lua, &net, opts).await
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
                if caps.auth_secrets().contains(&key) {
                    return Err(mlua::Error::runtime(format!(
                        "secret {key:?} is sent by tern itself (capabilities.auth), so the script can't read it; requests to its hosts already carry it"
                    )));
                }
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

    // Which account: `{ slug, label, account }`, the default when there are
    // no connections.
    let who = lua.create_table()?;
    match &net.conn {
        Some(r) => {
            who.raw_set("slug", r.conn.slug.clone())?;
            who.raw_set(
                "label",
                r.conn.label.clone().unwrap_or_else(|| r.conn.slug.clone()),
            )?;
            if let Some(a) = &r.conn.account {
                who.raw_set("account", a.clone())?;
            }
        }
        None => {
            who.raw_set("slug", tern_auth::DEFAULT_SLUG)?;
            who.raw_set("label", tern_auth::DEFAULT_SLUG)?;
        }
    }
    ctx.raw_set("connection", who)?;

    let kv = lua.create_table()?;
    let ns = Connections::kv_namespace(&name, net.conn.as_ref());
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

/// For a request to a hostname: resolve it, refuse non-public addresses, and
/// return a client pinned to exactly those addresses, so DNS can't answer
/// with a private one between the check and the connection. An IP literal is
/// checked directly and needs no pinning; a private one is allowed only when
/// the connector's own allowlist names that exact address.
async fn pinned_client(
    caps: &Capabilities,
    url: &reqwest::Url,
    host: &str,
) -> Result<Option<reqwest::Client>, String> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        let declared = caps
            .http
            .iter()
            .any(|h| entry_host(h).eq_ignore_ascii_case(bare));
        return if tern_core::net::is_public(ip) || declared {
            Ok(None)
        } else {
            Err(format!(
                "http: {host} is a private, loopback, or link-local address"
            ))
        };
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((bare, port))
        .await
        .map_err(|e| format!("http: resolving {host}: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("http: {host} did not resolve"));
    }
    if let Some(bad) = addrs.iter().find(|a| !tern_core::net::is_public(a.ip())) {
        return Err(format!(
            "http: refusing {host} ({}): private, loopback, and link-local addresses aren't allowed",
            bad.ip()
        ));
    }
    tern_core::net::http_client()
        .resolve_to_addrs(bare, &addrs)
        .build()
        .map(Some)
        .map_err(|e| format!("http client: {e}"))
}

/// Whether `url` is within the allowlist; `redirect` words the error for a hop.
fn check_target(caps: &Capabilities, url: &reqwest::Url, redirect: bool) -> mlua::Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(mlua::Error::runtime(if redirect {
            format!("http: redirect to unsupported scheme {:?}", url.scheme())
        } else {
            format!("http: unsupported scheme {:?}", url.scheme())
        }));
    }
    let host = url.host_str().unwrap_or_default();
    let allowed = if caps.http.is_empty() {
        "none".to_owned()
    } else {
        caps.http.join(", ")
    };
    if !host_allowed(&caps.http, host) {
        return Err(mlua::Error::runtime(if redirect {
            format!(
                "http: redirect to {host:?}, which is not in this connector's capabilities.http (allowed: {allowed})"
            )
        } else {
            format!(
                "http: host {host:?} is not in this connector's capabilities.http (allowed: {allowed})"
            )
        }));
    }
    if !path_allowed(&caps.http, host, url.path()) {
        let what = if redirect { "redirect to " } else { "" };
        return Err(mlua::Error::runtime(format!(
            "http: {what}{}{} is outside the paths capabilities.http allows (allowed: {allowed})",
            host,
            url.path()
        )));
    }
    Ok(())
}

/// Which declared credentials a request carries (`auth` in its options).
enum Pick {
    All,
    None,
    One(String),
}

impl Pick {
    fn wants(&self, auth: &Auth) -> bool {
        match self {
            Pick::All => true,
            Pick::None => false,
            Pick::One(name) => auth.secrets().contains(&name.as_str()),
        }
    }
}

/// Add one credential to a request. Every form of the value that could come
/// back (raw, URL-encoded, base64) goes into `redact`. An OAuth token sent
/// is returned, so a 401 can refresh past it.
async fn inject(
    net: &Net,
    auth: &Auth,
    headers: &mut HeaderMap,
    url: &mut reqwest::Url,
    redact: &mut Vec<String>,
    rejected: Option<&str>,
) -> mlua::Result<Option<String>> {
    let sensitive = |name: &str, value: String| -> mlua::Result<HeaderValue> {
        let mut v = HeaderValue::from_str(&value).map_err(|_| {
            mlua::Error::runtime(format!(
                "http: secret {name} can't be sent in a header (it has characters headers can't carry)"
            ))
        })?;
        v.set_sensitive(true);
        Ok(v)
    };
    match &auth.inject {
        Inject::Header {
            header,
            scheme,
            secret,
        } => {
            let value = net.secret(secret, rejected).await?;
            let full = if scheme.is_empty() {
                value.clone()
            } else {
                format!("{scheme} {value}")
            };
            let name = HeaderName::from_bytes(header.as_bytes())
                .map_err(|_| mlua::Error::runtime(format!("http: bad header {header:?}")))?;
            headers.insert(name, sensitive(secret, full)?);
            redact.push(value.clone());
            if is_oauth_secret(secret) {
                return Ok(Some(value));
            }
        }
        Inject::Query { param, secret } => {
            let value = net.secret(secret, rejected).await?;
            url.query_pairs_mut().append_pair(param, &value);
            redact.push(form_encode(&value));
            redact.push(value.clone());
            if is_oauth_secret(secret) {
                return Ok(Some(value));
            }
        }
        Inject::Basic { user, password } => {
            let u = match user {
                Some(n) => net.secret(n, None).await?,
                None => String::new(),
            };
            let p = match password {
                Some(n) => net.secret(n, None).await?,
                None => String::new(),
            };
            let token = base64(format!("{u}:{p}").as_bytes());
            headers.insert(
                header::AUTHORIZATION,
                sensitive("basic", format!("Basic {token}"))?,
            );
            redact.extend([u, p, token]);
        }
    }
    Ok(None)
}

/// Whether `url` contains any of `values`, as written or percent-decoded.
fn carries(url: &reqwest::Url, values: &[String]) -> bool {
    let raw = url.as_str();
    let decoded = percent_decode(raw);
    let plus = percent_decode(&raw.replace('+', " "));
    values.iter().any(|v| {
        !v.is_empty()
            && (raw.contains(v.as_str())
                || decoded.contains(v.as_str())
                || plus.contains(v.as_str()))
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| char::from(c).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Replace each injected value (longest first) with `[REDACTED]`.
fn scrub(values: &[String], text: &str) -> String {
    tern_core::secrets::redact_with(values, text)
}

/// `application/x-www-form-urlencoded`, as `Url::query_pairs_mut` writes it.
fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => {
                out.push(char::from(b));
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

async fn http_request(lua: &Lua, net: &Net, opts: Table) -> mlua::Result<Table> {
    let url: String = opts
        .raw_get::<Option<String>>("url")?
        .ok_or_else(|| mlua::Error::runtime("http.request: `url` is required"))?;
    let mut url = reqwest::Url::parse(&url)
        .map_err(|e| mlua::Error::runtime(format!("http: invalid url {url:?}: {e}")))?;
    check_target(&net.caps, &url, false)?;
    let method: String = opts
        .raw_get::<Option<String>>("method")?
        .unwrap_or_else(|| "GET".into());
    let mut method = reqwest::Method::from_bytes(method.to_ascii_uppercase().as_bytes())
        .map_err(|_| mlua::Error::runtime(format!("http: invalid method {method:?}")))?;
    // Checked before anything is sent: a `read` tool can't write by POST.
    if let Some(tool) = &net.get_only
        && !matches!(method, reqwest::Method::GET | reqwest::Method::HEAD)
    {
        return Err(mlua::Error::runtime(format!(
            "http: tools.{tool} declares effect = \"read\" but sent {method}; declare effect = \"write\", or set read_via_post = true if it only reads (GraphQL, search APIs)"
        )));
    }
    let pick = match opts.raw_get::<Value>("auth")? {
        Value::Nil | Value::Boolean(true) => Pick::All,
        Value::Boolean(false) => Pick::None,
        Value::String(s) => {
            let s = s.to_string_lossy();
            if !net.caps.auth_secrets().contains(&s) {
                return Err(mlua::Error::runtime(format!(
                    "http: auth = {s:?} isn't in this connector's capabilities.auth"
                )));
            }
            Pick::One(s)
        }
        _ => {
            return Err(mlua::Error::runtime(
                "http: `auth` must be false or a secret name from capabilities.auth",
            ));
        }
    };
    let mut headers = HeaderMap::new();
    if let Some(table) = opts.raw_get::<Option<Table>>("headers")? {
        for pair in table.pairs::<String, String>() {
            let (k, v) = pair?;
            let name = HeaderName::from_bytes(k.as_bytes())
                .map_err(|_| mlua::Error::runtime(format!("http: invalid header name {k:?}")))?;
            if name == header::HOST {
                return Err(mlua::Error::runtime(
                    "http: the Host header is set from the url",
                ));
            }
            let value = HeaderValue::from_str(&v)
                .map_err(|_| mlua::Error::runtime(format!("http: invalid value for header {k}")))?;
            headers.append(name, value);
        }
    }
    let json_body: Value = opts.raw_get("json")?;
    let mut body: Option<Vec<u8>> = if !json_body.is_nil() {
        let j: serde_json::Value = lua.from_value_with(json_body, FROM_LUA)?;
        headers
            .entry(header::CONTENT_TYPE)
            .or_insert(HeaderValue::from_static("application/json"));
        Some(serde_json::to_vec(&j).map_err(mlua::Error::external)?)
    } else {
        opts.raw_get::<Option<String>>("body")?
            .map(String::into_bytes)
    };
    let timeout = opts
        .raw_get::<Option<f64>>("timeout")?
        .map(|secs| Duration::from_secs_f64(secs.clamp(0.1, 300.0)));

    net.limiter
        .take(&net.connector, net.rate)
        .map_err(|e| mlua::Error::runtime(format!("http: {e}")))?;

    // Redirects are followed here, not by reqwest, so every hop gets the
    // allowlist, path and address checks, and credentials are added afresh
    // for the hop's own host (or not at all).
    let first_host = url.host_str().unwrap_or_default().to_owned();
    let mut redact: Vec<String> = Vec::new();
    // Each value sent, by the index of the `auth` entry that sent it.
    let mut sent_by: Vec<(usize, Vec<String>)> = Vec::new();
    let mut hops = 0;
    let (resp, host) = loop {
        let host = url.host_str().unwrap_or_default().to_owned();
        if hops > 0 {
            check_target(&net.caps, &url, true)?;
            // A server can copy a credential into `Location` (a query key
            // most easily); only a host its entry is sent to may get it.
            let leaks = sent_by.iter().any(|(i, values)| {
                let a = &net.caps.auth[*i];
                !(a.applies_to(&host) && pick.wants(a)) && carries(&url, values)
            });
            if leaks {
                return Err(mlua::Error::runtime(format!(
                    "http: redirect to {host:?} refused: its url carries a credential from capabilities.auth that isn't sent to that host"
                )));
            }
        }
        let pinned = pinned_client(&net.caps, &url, &host)
            .await
            .map_err(mlua::Error::runtime)?;
        let client = pinned.as_ref().unwrap_or(&net.client);
        let mut hop_headers = headers.clone();
        if !host.eq_ignore_ascii_case(&first_host) {
            for h in [
                header::AUTHORIZATION,
                header::COOKIE,
                header::PROXY_AUTHORIZATION,
            ] {
                hop_headers.remove(h);
            }
        }
        // An OAuth token the service refuses (401) is refreshed and the hop
        // sent once more.
        let mut rejected: Option<String> = None;
        let resp = loop {
            let mut hop_headers = hop_headers.clone();
            let mut hop_url = url.clone();
            let mut sent_token = None;
            for (i, auth) in net
                .caps
                .auth
                .iter()
                .enumerate()
                .filter(|(_, a)| a.applies_to(&host) && pick.wants(a))
            {
                let mut values = Vec::new();
                let sent = inject(
                    net,
                    auth,
                    &mut hop_headers,
                    &mut hop_url,
                    &mut values,
                    rejected.as_deref(),
                )
                .await?;
                sent_token = sent_token.or(sent);
                values.retain(|v| v.len() >= 4);
                redact.extend(values.iter().cloned());
                sent_by.push((i, values));
            }
            redact.retain(|v| v.len() >= 4);
            redact.sort_by_key(|v| std::cmp::Reverse(v.len()));
            redact.dedup();
            let mut req = client.request(method.clone(), hop_url).headers(hop_headers);
            if let Some(b) = &body {
                req = req.body(b.clone());
            }
            if let Some(t) = timeout {
                req = req.timeout(t);
            }
            let resp = req.send().await.map_err(|e| {
                mlua::Error::runtime(scrub(
                    &redact,
                    &format!("http: request to {host} failed: {}", error_chain(&e)),
                ))
            })?;
            match sent_token {
                Some(t)
                    if resp.status() == reqwest::StatusCode::UNAUTHORIZED && rejected.is_none() =>
                {
                    rejected = Some(t);
                }
                _ => break resp,
            }
        };
        let location = resp
            .status()
            .is_redirection()
            .then(|| resp.headers().get(header::LOCATION))
            .flatten()
            .and_then(|l| l.to_str().ok())
            .map(str::to_owned);
        let Some(location) = location else {
            break (resp, host);
        };
        hops += 1;
        if hops > MAX_REDIRECTS {
            return Err(mlua::Error::runtime(format!(
                "http: more than {MAX_REDIRECTS} redirects"
            )));
        }
        url = url.join(&location).map_err(|e| {
            mlua::Error::runtime(scrub(
                &redact,
                &format!("http: bad redirect {location:?}: {e}"),
            ))
        })?;
        let status = resp.status().as_u16();
        if status == 303 || (matches!(status, 301 | 302) && method == reqwest::Method::POST) {
            method = reqwest::Method::GET;
            body = None;
            headers.remove(header::CONTENT_TYPE);
        }
    };
    let status = resp.status().as_u16();
    if matches!(status, 429 | 503)
        && let Some(secs) = resp
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
    {
        net.limiter
            .retry_after(&net.connector, Duration::from_secs(secs));
    }
    let headers = lua.create_table()?;
    for (k, v) in resp.headers() {
        if let Ok(v) = v.to_str() {
            headers.raw_set(k.as_str(), scrub(&redact, v))?;
        }
    }
    let mut raw = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            mlua::Error::runtime(scrub(
                &redact,
                &format!("http: reading body from {host}: {e}"),
            ))
        })?;
        if raw.len() + chunk.len() > MAX_HTTP_BODY {
            return Err(mlua::Error::runtime(format!(
                "http: response body exceeds {MAX_HTTP_BODY} bytes"
            )));
        }
        raw.extend_from_slice(&chunk);
    }
    let text = scrub(&redact, &String::from_utf8_lossy(&raw));
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
    fn encodings_match_what_is_sent() {
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b""), "");
        let mut u = reqwest::Url::parse("http://x/").unwrap();
        let v = "a b/c+d=é";
        u.query_pairs_mut().append_pair("k", v);
        assert_eq!(u.query().unwrap(), format!("k={}", form_encode(v)));
    }
}
