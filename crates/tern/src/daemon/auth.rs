//! Who is calling, and what they may do (deployment spec §6.2).
//!
//! The Unix socket is the owner: whoever can open it (mode 0600) needs no
//! token. TCP listeners put [`require_auth`] in front of the same router,
//! so route handlers don't change: every request there needs a device token
//! (`Authorization: Bearer tern_dev_…`) whose scope covers the route, except
//! `/healthz` and `POST /v1/auth/pair` (the pairing code is the credential).
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/healthz` | liveness, no auth, no details |
//! | POST | `/v1/auth/pair` | redeem `{code, device_name?, platform?}` for `{device_id, token, scope}` |
//! | POST | `/v1/devices/pair` | owner: make a code `{scope?, name?}` → `{code, link, fingerprint?, expires_in_secs}` |
//! | GET | `/v1/devices` | owner: paired devices |
//! | PATCH | `/v1/devices/{id}` | owner: rename `{name}` |
//! | DELETE | `/v1/devices/{id}` | owner: revoke, closing its event streams |

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, Path as UrlPath, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Extension, Json, Router};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tern_core::TernHome;
use tern_core::config::{ApiConfig, TlsMode};
use tern_store::{DeviceGrant, DeviceRow, Store};
use tokio_util::sync::CancellationToken;

use super::manager::Manager;

type Shared = Arc<Manager>;

/// Bumped when `/v1` changes incompatibly.
pub const API_VERSION: u32 = 1;

/// What `/v1/health` advertises; clients derive behaviour from it.
pub const FEATURES: &[&str] = &[
    "devices",
    // `/v1/providers`, `/v1/models` and `/v1/config/default_model`.
    "providers",
    "schedules",
    "uploads",
    "subagents",
    "secret_requests",
    "mcp",
    // `/v1/channels` and Home's stream (docs/channels-design.md).
    "channels",
    // `/v1/agents`: agent profiles, each with its own DM
    // (docs/agent-profiles-design.md).
    "agents",
    // `/v1/outbox` and `outbox_changed` events (tooling spec §5.2).
    "outbox",
    // `/v1/webhooks`: webhook routines, their secrets and local fires (T1c).
    "webhooks",
    // `/v1/connections`, `/v1/auth-prompts` and their events (tooling spec
    // §6.7–6.8).
    "connections",
    // `/v1/ledger`: the hash-chained activity ledger (tooling spec §8.1).
    "ledger",
    // `/v1/tasks`: everything running, queued, scheduled, waiting on you or
    // failed recently (tooling spec §8.2).
    "tasks",
    // `/v1/waits`: active `wait_until` waits, and cancelling one.
    "waits",
    // `/v1/security/audit`: `tern security audit` (tooling spec §8.4).
    "security_audit",
    // `/v1/bundles`: preview, install, remove (tooling spec §6.5).
    "bundles",
    // `/v1/skills` and `/v1/skill-offers`: skill capture (tooling spec §7, T5b).
    "skill_capture",
    // `/v1/workspaces`: the folder an agent can reach, chosen per Home topic,
    // channel and agent, with read-only extra folders
    // (docs/alpha-workspaces.md). Older cores lack it: hide the folder picker.
    "workspaces",
];

pub const TOKEN_PREFIX: &str = "tern_dev_";
const CODE_TTL: Duration = Duration::from_secs(10 * 60);
const PAIR_ATTEMPTS_PER_MINUTE: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// View only.
    Read,
    /// Conversations, approvals, schedules, memories; no config, secrets,
    /// devices, or connector changes.
    Chat,
    /// Everything.
    Owner,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Chat => "chat",
            Self::Owner => "owner",
        }
    }
}

impl std::str::FromStr for Scope {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        match s {
            "read" => Ok(Self::Read),
            "chat" => Ok(Self::Chat),
            "owner" => Ok(Self::Owner),
            other => anyhow::bail!("scope {other:?} must be owner, chat, or read"),
        }
    }
}

/// The caller, as a request extension. Absent on the Unix socket, which
/// means the local owner.
#[derive(Debug, Clone)]
pub enum Auth {
    Device { id: String, scope: Scope },
}

/// Paths only the owner may touch, whatever the method.
const OWNER_ONLY: &[&str] = &[
    "/v1/devices",
    "/v1/config",
    "/v1/secrets",
    // Accounts and sign-in cards: credentials, like secrets.
    "/v1/connections",
    "/v1/auth-prompts",
    "/v1/providers",
    "/v1/shutdown",
    // Moving conversations between channels rearranges what everyone sees.
    "/v1/channels/move",
    // Webhook secrets let the internet start (tainted) runs.
    "/v1/webhooks",
    // Every account, grant and approval (tooling spec §8.1).
    "/v1/ledger",
    // How exposed the core is (tooling spec §8.4).
    "/v1/security",
    // Installing code and routines, and fetching URLs from this machine.
    "/v1/bundles",
];

/// Paths a `chat` device may change. Any other non-GET route is owner-only,
/// so a new endpoint is safe until it's listed here.
const CHAT_WRITES: &[&str] = &[
    "/v1/runs",
    "/v1/sessions",
    "/v1/approvals",
    "/v1/outbox",
    "/v1/schedules",
    "/v1/waits",
    "/v1/memories",
    "/v1/uploads",
    "/v1/models/check",
];

fn under(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The least scope that may call `method path`.
pub fn required_scope(method: &Method, path: &str) -> Scope {
    if OWNER_ONLY.iter().any(|p| under(path, p)) {
        Scope::Owner
    } else if method == Method::GET || method == Method::HEAD {
        Scope::Read
    } else if CHAT_WRITES.iter().any(|p| under(path, p)) {
        Scope::Chat
    } else {
        Scope::Owner
    }
}

/// Shared by every TCP listener.
#[derive(Clone)]
pub struct AuthState {
    pub manager: Shared,
    pub trust_proxy: bool,
    pairing: Arc<RateLimit>,
}

impl AuthState {
    pub fn new(manager: Shared, trust_proxy: bool) -> Self {
        Self {
            manager,
            trust_proxy,
            pairing: Arc::new(RateLimit::new(
                PAIR_ATTEMPTS_PER_MINUTE,
                Duration::from_secs(60),
            )),
        }
    }
}

/// The peer of a TCP connection.
#[derive(Debug, Clone, Copy)]
pub struct Peer(pub SocketAddr);

fn deny(status: StatusCode, msg: &str) -> Response {
    let mut resp = (status, Json(json!({ "error": msg }))).into_response();
    if status == StatusCode::UNAUTHORIZED {
        resp.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Bearer"),
        );
    }
    resp
}

/// The client's address: the peer, or with `trust_proxy` and a loopback
/// peer, the last `X-Forwarded-For` entry. A proxy appends the address it
/// saw to whatever the client sent, so the first entry is the client's own
/// claim and any value there could be made up per request.
pub(super) fn client_ip(peer: SocketAddr, headers: &HeaderMap, trust_proxy: bool) -> IpAddr {
    if trust_proxy
        && peer.ip().is_loopback()
        && let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse().ok())
    {
        return ip;
    }
    peer.ip()
}

pub fn hash_token(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
}

/// The middleware for TCP listeners.
pub async fn require_auth(
    State(st): State<AuthState>,
    ConnectInfo(Peer(peer)): ConnectInfo<Peer>,
    mut req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_owned();
    let ip = client_ip(peer, req.headers(), st.trust_proxy);
    if path == "/healthz" {
        return next.run(req).await;
    }
    if path == "/v1/auth/pair" {
        if !st.pairing.allow(ip) {
            tracing::warn!(target: "audit", %ip, "pairing rate limit hit");
            return deny(
                StatusCode::TOO_MANY_REQUESTS,
                "too many pairing attempts; wait a minute",
            );
        }
        return next.run(req).await;
    }
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    if token.is_some_and(|t| t.starts_with(super::hooks::SECRET_PREFIX)) {
        // W6: a webhook secret is for its hook's path on the webhook
        // listener, never a credential for the API.
        tracing::warn!(target: "audit", %ip, %path, "webhook secret sent to the API; refused");
        return deny(
            StatusCode::UNAUTHORIZED,
            "webhook secrets are not accepted on the API; use a device token",
        );
    }
    let Some(token) = token.filter(|t| t.starts_with(TOKEN_PREFIX)) else {
        return deny(
            StatusCode::UNAUTHORIZED,
            "a device token is required; pair this device first",
        );
    };
    let store = &st.manager.rt.store;
    let device = match store.device_by_token_hash(&hash_token(token)) {
        Ok(Some(d)) => d,
        Ok(None) => {
            return deny(
                StatusCode::UNAUTHORIZED,
                "this device was signed out (token revoked or unknown)",
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "device lookup failed");
            return deny(StatusCode::INTERNAL_SERVER_ERROR, "device lookup failed");
        }
    };
    let Ok(scope) = device.scope.parse::<Scope>() else {
        return deny(StatusCode::UNAUTHORIZED, "this device has an unknown scope");
    };
    let method = req.method().clone();
    let needed = required_scope(&method, &path);
    if scope < needed {
        return deny(
            StatusCode::FORBIDDEN,
            &format!(
                "this device has {} scope; {method} {path} needs {}",
                scope.as_str(),
                needed.as_str()
            ),
        );
    }
    if let Err(e) = store.touch_device(&device.id, Some(&ip.to_string())) {
        tracing::warn!(error = %e, "recording device activity failed");
    }
    if needed == Scope::Owner && method != Method::GET {
        tracing::info!(target: "audit", device = %device.id, %ip, %method, %path, "owner action from a device");
    }
    req.extensions_mut().insert(Auth::Device {
        id: device.id.clone(),
        scope,
    });
    let resp = next.run(req).await;
    // Event streams end when the device is revoked.
    let streaming = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"text/event-stream"));
    if !streaming {
        return resp;
    }
    let revoked = STREAMS.token(&device.id);
    resp.map(|body| {
        Body::from_stream(
            body.into_data_stream()
                .take_until(revoked.cancelled_owned()),
        )
    })
}

/// Per-device cancellation for open event streams. Process-wide, because
/// both the TCP middleware and the device routes (on every listener) use it.
static STREAMS: LazyLock<Streams> = LazyLock::new(Streams::default);

#[derive(Default)]
struct Streams(Mutex<HashMap<String, CancellationToken>>);

impl Streams {
    fn token(&self, device: &str) -> CancellationToken {
        let mut map = self.0.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(device.to_owned()).or_default().clone()
    }

    fn revoke(&self, device: &str) {
        let mut map = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = map.remove(device) {
            t.cancel();
        }
    }
}

/// A sliding-window limit per key (an IP, by default).
pub(super) struct RateLimit<K = IpAddr> {
    max: usize,
    window: Duration,
    hits: Mutex<HashMap<K, VecDeque<Instant>>>,
}

impl<K: std::hash::Hash + Eq> RateLimit<K> {
    pub(super) fn new(max: usize, window: Duration) -> Self {
        Self {
            max,
            window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn allow(&self, key: K) -> bool {
        self.allow_up_to(key, self.max)
    }

    /// Whether `key` has used up its window, without counting a hit.
    pub(super) fn exceeded(&self, key: &K) -> bool {
        let now = Instant::now();
        let hits = self.hits.lock().unwrap_or_else(|p| p.into_inner());
        hits.get(key).is_some_and(|q| {
            q.iter()
                .filter(|t| now.duration_since(**t) < self.window)
                .count()
                >= self.max
        })
    }

    /// [`Self::allow`] with a limit that may change between calls.
    pub(super) fn allow_up_to(&self, key: K, max: usize) -> bool {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap_or_else(|p| p.into_inner());
        hits.retain(|_, q| {
            while q
                .front()
                .is_some_and(|t| now.duration_since(*t) >= self.window)
            {
                q.pop_front();
            }
            !q.is_empty()
        });
        let q = hits.entry(key).or_default();
        if q.len() >= max {
            return false;
        }
        q.push_back(now);
        true
    }
}

// ---- tokens, codes, links ----

const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// `tern_dev_` and 43 base62 characters (256 bits) from the OS CSPRNG.
pub fn new_token() -> anyhow::Result<String> {
    new_secret(TOKEN_PREFIX)
}

/// `prefix` and 43 base62 characters (256 bits) from the OS CSPRNG.
pub fn new_secret(prefix: &str) -> anyhow::Result<String> {
    let mut out = String::from(prefix);
    let mut buf = [0u8; 64];
    while out.len() < prefix.len() + 43 {
        aws_lc_rs::rand::fill(&mut buf).map_err(|_| anyhow::anyhow!("no randomness"))?;
        // 248 = 4 × 62: rejecting the rest keeps every symbol equally likely.
        for b in buf.iter().filter(|b| **b < 248) {
            if out.len() == prefix.len() + 43 {
                break;
            }
            out.push(BASE62[usize::from(*b % 62)] as char);
        }
    }
    Ok(out)
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The URL devices reach this core at: `api.public_url`, else the first TCP
/// listener (this host's name when it listens on every address).
pub fn public_url(api: &ApiConfig) -> anyhow::Result<Option<String>> {
    if let Some(u) = &api.public_url {
        return Ok(Some(u.trim_end_matches('/').to_owned()));
    }
    let Some(addr) = super::listen::tcp_addrs(api)?.into_iter().next() else {
        return Ok(None);
    };
    let scheme = if api.tls == TlsMode::Off {
        "http"
    } else {
        "https"
    };
    let host = match addr.ip() {
        ip if ip.is_unspecified() => super::tls::host_name(),
        IpAddr::V6(ip) => format!("[{ip}]"),
        IpAddr::V4(ip) => ip.to_string(),
    };
    Ok(Some(format!("{scheme}://{host}:{}", addr.port())))
}

/// A pairing code ready to hand to a device.
#[derive(Debug, Serialize, Deserialize)]
pub struct PairingOffer {
    pub code: String,
    pub scope: Scope,
    /// `tern://pair?u=…&c=…&fp=…&n=…`, when a TCP listener or public URL is
    /// configured.
    pub link: Option<String>,
    pub url: Option<String>,
    pub fingerprint: Option<String>,
    pub expires_in_secs: u64,
}

/// Create a single-use device pairing code (spec §6.2 step 1).
pub fn offer_pairing(
    home: &TernHome,
    api: &ApiConfig,
    store: &Store,
    scope: Scope,
    name: Option<String>,
    url: Option<String>,
) -> anyhow::Result<PairingOffer> {
    let code = crate::gateway_cmd::new_code();
    store.create_device_pairing_code(
        &code,
        &DeviceGrant {
            scope: scope.as_str().to_owned(),
            name,
        },
        i64::try_from(CODE_TTL.as_millis())?,
    )?;
    let url = match url {
        Some(u) => Some(u.trim_end_matches('/').to_owned()),
        None => public_url(api)?,
    };
    let fingerprint = super::tls::pin(home, api)?;
    let link = url.as_ref().map(|u| {
        let mut link = format!("tern://pair?u={}&c={code}", percent_encode(u));
        if let Some(fp) = &fingerprint {
            link.push_str(&format!("&fp={}", percent_encode(fp)));
        }
        link.push_str(&format!("&n={}", percent_encode(&super::tls::host_name())));
        link
    });
    Ok(PairingOffer {
        code,
        scope,
        link,
        url,
        fingerprint,
        expires_in_secs: CODE_TTL.as_secs(),
    })
}

/// Fields of a `tern://pair` link.
#[derive(Debug, PartialEq, Eq)]
pub struct PairLink {
    pub url: String,
    pub code: String,
    pub fingerprint: Option<String>,
    pub name: Option<String>,
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl std::str::FromStr for PairLink {
    type Err = anyhow::Error;
    fn from_str(link: &str) -> anyhow::Result<Self> {
        let query = link
            .strip_prefix("tern://pair?")
            .ok_or_else(|| anyhow::anyhow!("a pairing link starts with tern://pair?"))?;
        let mut fields: HashMap<&str, String> = HashMap::new();
        for pair in query.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                fields.insert(k, percent_decode(v));
            }
        }
        let url = fields
            .remove("u")
            .ok_or_else(|| anyhow::anyhow!("the pairing link has no core URL (u=)"))?;
        let code = fields
            .remove("c")
            .ok_or_else(|| anyhow::anyhow!("the pairing link has no code (c=)"))?;
        Ok(Self {
            url,
            code,
            fingerprint: fields.remove("fp"),
            name: fields.remove("n"),
        })
    }
}

// ---- routes ----

/// Routes this module adds to the API router.
pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/auth/pair", post(pair))
        .route("/v1/devices", get(list_devices))
        .route("/v1/devices/pair", post(offer))
        .route(
            "/v1/devices/{id}",
            patch(rename_device).delete(revoke_device),
        )
        .merge(super::uploads::routes())
}

/// `/v1/health`: who you are to this core and what it can do (spec §7.3).
pub fn health(auth: Option<&Auth>) -> Value {
    let (deployment, kind, scope) = match auth {
        None => ("local", "owner", Scope::Owner),
        Some(Auth::Device { scope, .. }) => ("server", "device", *scope),
    };
    json!({
        "pid": std::process::id(),
        "version": env!("CARGO_PKG_VERSION"),
        "api": API_VERSION,
        "deployment": deployment,
        "store": "sqlite",
        "library": "fs",
        "auth": { "kind": kind, "scope": scope },
        "features": FEATURES,
    })
}

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
}

#[derive(Serialize, Deserialize)]
pub struct PairBody {
    pub code: String,
    pub device_name: Option<String>,
    pub platform: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Paired {
    pub device_id: String,
    pub token: String,
    pub scope: Scope,
}

async fn pair(State(m): State<Shared>, Json(b): Json<PairBody>) -> Result<Json<Paired>, ApiError> {
    let store = &m.rt.store;
    let code = b.code.trim().to_ascii_uppercase();
    let Some(grant) = store.consume_device_pairing_code(&code).map_err(internal)? else {
        tracing::warn!(target: "audit", "device pairing with an invalid or expired code");
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "the pairing code is invalid, used, or expired".into(),
        ));
    };
    let scope: Scope = grant.scope.parse().map_err(internal)?;
    let name = grant
        .name
        .or(b.device_name)
        .map(|n| n.trim().chars().take(100).collect::<String>())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "device".into());
    let token = new_token().map_err(internal)?;
    let device = DeviceRow {
        id: uuid::Uuid::now_v7().to_string(),
        name,
        platform: b.platform.map(|p| p.chars().take(40).collect()),
        scope: scope.as_str().to_owned(),
        created_at: tern_store::now_ms(),
        last_seen_at: None,
        last_ip: None,
        revoked_at: None,
    };
    store
        .create_device(&device, &hash_token(&token))
        .map_err(internal)?;
    tracing::info!(target: "audit", device = %device.id, name = %device.name, scope = scope.as_str(), "device paired");
    Ok(Json(Paired {
        device_id: device.id,
        token,
        scope,
    }))
}

#[derive(Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub platform: Option<String>,
    pub scope: String,
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
    pub last_ip: Option<String>,
    pub revoked_at: Option<i64>,
}

impl From<DeviceRow> for Device {
    fn from(d: DeviceRow) -> Self {
        Self {
            id: d.id,
            name: d.name,
            platform: d.platform,
            scope: d.scope,
            created_at: d.created_at,
            last_seen_at: d.last_seen_at,
            last_ip: d.last_ip,
            revoked_at: d.revoked_at,
        }
    }
}

async fn list_devices(State(m): State<Shared>) -> Result<Json<Vec<Device>>, ApiError> {
    let devices = m.rt.store.devices().map_err(internal)?;
    Ok(Json(devices.into_iter().map(Device::from).collect()))
}

#[derive(Deserialize)]
struct OfferBody {
    scope: Option<Scope>,
    name: Option<String>,
}

async fn offer(
    State(m): State<Shared>,
    Json(b): Json<OfferBody>,
) -> Result<Json<PairingOffer>, ApiError> {
    let offer = offer_pairing(
        &m.rt.home,
        &m.rt.config.api,
        &m.rt.store,
        b.scope.unwrap_or(Scope::Chat),
        b.name,
        None,
    )
    .map_err(internal)?;
    Ok(Json(offer))
}

#[derive(Deserialize)]
struct RenameBody {
    name: String,
}

async fn rename_device(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<RenameBody>,
) -> Result<Json<Value>, ApiError> {
    if m.rt
        .store
        .rename_device(&id, b.name.trim())
        .map_err(internal)?
    {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(ApiError(StatusCode::NOT_FOUND, "device not found".into()))
    }
}

async fn revoke_device(
    State(m): State<Shared>,
    auth: Option<Extension<Auth>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let revoked = m.rt.store.revoke_device(&id).map_err(internal)?;
    STREAMS.revoke(&id);
    let by = match auth {
        Some(Extension(Auth::Device { id, .. })) => id,
        None => "local".into(),
    };
    tracing::info!(target: "audit", device = %id, by = %by, revoked, "device revoked");
    if revoked {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(ApiError(
            StatusCode::NOT_FOUND,
            "no such active device".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_per_route() {
        let get = Method::GET;
        let post = Method::POST;
        assert_eq!(required_scope(&get, "/v1/sessions"), Scope::Read);
        assert_eq!(required_scope(&get, "/v1/events"), Scope::Read);
        assert_eq!(required_scope(&post, "/v1/runs"), Scope::Chat);
        assert_eq!(required_scope(&post, "/v1/approvals/abc"), Scope::Chat);
        assert_eq!(
            required_scope(&Method::PUT, "/v1/sessions/x/model"),
            Scope::Chat
        );
        assert_eq!(required_scope(&post, "/v1/schedules"), Scope::Chat);
        assert_eq!(required_scope(&get, "/v1/devices"), Scope::Owner);
        assert_eq!(
            required_scope(&Method::DELETE, "/v1/devices/x"),
            Scope::Owner
        );
        assert_eq!(required_scope(&get, "/v1/secrets"), Scope::Owner);
        assert_eq!(required_scope(&post, "/v1/shutdown"), Scope::Owner);
        // Webhook secrets let the internet start runs: owner-only to read too.
        assert_eq!(required_scope(&get, "/v1/webhooks"), Scope::Owner);
        assert_eq!(required_scope(&post, "/v1/webhooks/x/rotate"), Scope::Owner);
        assert_eq!(required_scope(&get, "/v1/status"), Scope::Read);
        assert_eq!(required_scope(&get, "/v1/mcp/github"), Scope::Read);
        assert_eq!(required_scope(&post, "/v1/mcp"), Scope::Owner);
        assert_eq!(required_scope(&post, "/v1/mcp/import"), Scope::Owner);
        assert_eq!(
            required_scope(&Method::PATCH, "/v1/mcp/github"),
            Scope::Owner
        );
        // Unlisted writes are owner-only, and prefixes match whole segments.
        assert_eq!(
            required_scope(&post, "/v1/connectors/x/activate"),
            Scope::Owner
        );
        assert_eq!(required_scope(&post, "/v1/runsx"), Scope::Owner);
        assert_eq!(required_scope(&get, "/v1/devicesx"), Scope::Read);
        assert!(Scope::Owner > Scope::Chat && Scope::Chat > Scope::Read);
    }

    #[test]
    fn tokens_are_prefixed_base62_and_distinct() {
        let a = new_token().unwrap();
        let b = new_token().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), TOKEN_PREFIX.len() + 43);
        assert!(
            a[TOKEN_PREFIX.len()..]
                .bytes()
                .all(|c| c.is_ascii_alphanumeric())
        );
    }

    #[test]
    fn rate_limit_is_per_ip() {
        let rl = RateLimit::new(2, Duration::from_secs(60));
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        assert!(rl.allow(a) && rl.allow(a));
        assert!(!rl.allow(a));
        assert!(rl.allow(b));
    }

    #[test]
    fn forwarded_for_only_from_a_trusted_loopback_proxy() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.9, 10.0.0.1".parse().unwrap());
        let lo: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let lan: SocketAddr = "192.168.1.4:5000".parse().unwrap();
        // The proxy appends the address it saw, so the last entry is the client.
        assert_eq!(client_ip(lo, &h, true).to_string(), "10.0.0.1");
        assert_eq!(client_ip(lo, &h, false), lo.ip());
        assert_eq!(client_ip(lan, &h, true), lan.ip());
    }

    #[test]
    fn pair_links_round_trip() {
        let link = format!(
            "tern://pair?u={}&c=K7QX9M2P&fp={}&n={}",
            percent_encode("https://home.example.ts.net:7420"),
            percent_encode("sha256:AB12"),
            percent_encode("Mitch's Mac"),
        );
        let p: PairLink = link.parse().unwrap();
        assert_eq!(
            p,
            PairLink {
                url: "https://home.example.ts.net:7420".into(),
                code: "K7QX9M2P".into(),
                fingerprint: Some("sha256:AB12".into()),
                name: Some("Mitch's Mac".into()),
            }
        );
        assert!("https://x".parse::<PairLink>().is_err());
        assert!("tern://pair?c=X".parse::<PairLink>().is_err());
    }

    #[test]
    fn public_url_from_listeners() {
        let mut api = ApiConfig::default();
        assert_eq!(public_url(&api).unwrap(), None);
        api.listen = vec!["unix".into(), "tcp://127.0.0.1:7420".into()];
        assert_eq!(
            public_url(&api).unwrap().as_deref(),
            Some("https://127.0.0.1:7420")
        );
        api.public_url = Some("https://tern.example.com/".into());
        assert_eq!(
            public_url(&api).unwrap().as_deref(),
            Some("https://tern.example.com")
        );
    }
}
