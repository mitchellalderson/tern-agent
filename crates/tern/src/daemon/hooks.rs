//! Webhook triggers (tooling spec §3.1, T1c; wargame W6): the one route the
//! internet may call, on a listener of its own.
//!
//! `[webhooks] listen` binds TCP listeners that serve only this router:
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/healthz` | `tern webhooks`, no auth (for `tern doctor` and probes) |
//! | POST | `/v1/hooks/{routine-id}` | a delivery; 202 queued, 200 duplicate or ignored |
//!
//! Every other path is 404, so no API route is reachable here. A delivery
//! needs the routine's own secret, by its kind:
//! - `hmac`: `X-Tern-Signature: t=<unix secs>,v1=<hex HMAC-SHA256 of "<t>.<body>">`
//!   (the timestamp must be within `tolerance_secs`), or GitHub's
//!   `X-Hub-Signature-256: sha256=<hex HMAC-SHA256 of the body>`;
//! - `bearer`: `Authorization: Bearer tern_whk_…`.
//!
//! A device token is refused here (401, audited), and a webhook secret is
//! refused on the API (`auth::require_auth`). Refusals, in order: a device
//! token or unknown hook 404/401, a body over `max_body_kb` 413, a bad or
//! missing signature or a stale timestamp 401, over `per_minute` deliveries
//! for the routine 429, the routine turned off 409. Twenty failures a
//! minute from one address get 429 until the minute passes.
//!
//! Connections are held to `header_timeout_secs` for the TLS handshake and
//! each request's headers (idle keep-alive included), `body_timeout_secs`
//! for a body (408), and `max_connections` open at once (503, or closed
//! under TLS): a slow or idle client can't hold the port.
//!
//! An accepted delivery becomes a `routine_events` row keyed by a hash of
//! what was signed (the body, plus the timestamp for `X-Tern-Signature`),
//! so a re-delivery or a replay inside the window starts nothing. The
//! routine's gate, if it has one, sees the event first and can drop or
//! reshape it, with no model call. Its run starts tainted, with the event
//! wrapped as untrusted, exactly like a watch's.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path as UrlPath, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use serde_json::{Value, json};
use tern_core::TernHome;
use tern_core::config::{ApiConfig, Config, TlsMode, WebhooksConfig};
use tern_store::ScheduleRow;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;

use super::auth::{self, Peer, RateLimit};
use super::listen::{self, Io};
use super::manager::Manager;

/// Webhook secrets start with this, so the API can tell one apart and
/// refuse it by name.
pub const SECRET_PREFIX: &str = "tern_whk_";
/// `NewRun.taint` for a run a delivery started.
pub const TAINT: &str = "started by a webhook";
/// Where a routine's hook lives on the webhook listener.
pub const PATH_PREFIX: &str = "/v1/hooks/";
/// Failed deliveries one address may make per minute.
const FAILURES_PER_MINUTE: usize = 20;
/// Headers kept on the event: the kind of event and its id, which senders
/// put in headers rather than the body. Never credentials.
const KEPT_HEADERS: &[&str] = &["content-type", "user-agent"];

/// `hmac` or `bearer`.
pub fn check_auth(kind: &str) -> anyhow::Result<&'static str> {
    match kind.trim() {
        "" | "hmac" => Ok("hmac"),
        "bearer" => Ok("bearer"),
        other => bail!("webhook auth {other:?} must be hmac or bearer"),
    }
}

/// A new secret: `tern_whk_` and 43 base62 characters.
pub fn new_secret() -> anyhow::Result<String> {
    auth::new_secret(SECRET_PREFIX)
}

/// The listener addresses, refusing plain HTTP off loopback (as the API
/// does: a bearer secret must never cross a network in clear).
pub fn check(config: &Config) -> anyhow::Result<Vec<SocketAddr>> {
    let addrs = listen::parse_tcp(&config.webhooks.listen, "webhooks.listen")?;
    if tls_mode(config) == TlsMode::Off
        && !addrs.is_empty()
        && !(config.api.trust_proxy && addrs.iter().all(|a| a.ip().is_loopback()))
    {
        bail!(
            "webhooks with TLS off are only allowed when every webhooks.listen entry is on loopback and api.trust_proxy = true (a TLS proxy or tunnel in front)"
        );
    }
    Ok(addrs)
}

fn tls_mode(config: &Config) -> TlsMode {
    config.webhooks.tls.unwrap_or(config.api.tls)
}

/// The base URL senders use: `public_url`, else the first listener.
pub fn base_url(config: &Config) -> Option<String> {
    if let Some(u) = &config.webhooks.public_url {
        return Some(u.trim_end_matches('/').to_owned());
    }
    let addr = check(config).ok()?.into_iter().next()?;
    let api = ApiConfig {
        listen: vec![format!("tcp://{addr}")],
        tls: tls_mode(config),
        ..ApiConfig::default()
    };
    auth::public_url(&api).ok().flatten()
}

/// The full URL of routine `id`'s hook, when a listener or public URL is
/// configured.
pub fn url(config: &Config, id: &str) -> Option<String> {
    base_url(config).map(|b| format!("{b}{PATH_PREFIX}{id}"))
}

struct Hooks {
    manager: Arc<Manager>,
    trust_proxy: bool,
    failures: RateLimit,
    accepted: RateLimit<String>,
}

/// Bind every webhook listener and serve until `shutdown`.
pub async fn serve(
    manager: Arc<Manager>,
    home: &TernHome,
    config: &Config,
    shutdown: CancellationToken,
) -> anyhow::Result<Vec<(SocketAddr, tokio::task::JoinHandle<()>)>> {
    let addrs = check(config)?;
    if addrs.is_empty() {
        return Ok(Vec::new());
    }
    let api = ApiConfig {
        tls: tls_mode(config),
        ..config.api.clone()
    };
    let tls = super::tls::server_config(home, &api)?.map(TlsAcceptor::from);
    let state = Arc::new(Hooks {
        manager,
        trust_proxy: config.api.trust_proxy,
        failures: RateLimit::new(FAILURES_PER_MINUTE, Duration::from_secs(60)),
        accepted: RateLimit::new(1, Duration::from_secs(60)),
    });
    let app = router(state);
    let limits = Limits::new(&config.webhooks);
    let mut out = Vec::new();
    for addr in addrs {
        let tcp = TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding webhooks listener {addr}"))?;
        let local = tcp.local_addr()?;
        let task = tokio::spawn(accept_loop(
            tcp,
            tls.clone(),
            app.clone(),
            limits.clone(),
            shutdown.clone(),
        ));
        tracing::info!(addr = %local, tls = tls.is_some(), "webhooks listening on TCP");
        out.push((local, task));
    }
    Ok(out)
}

/// What a connection may take before the listener drops it: the slowloris
/// guard on the one port meant for the internet.
#[derive(Clone)]
struct Limits {
    /// For the TLS handshake, then for each request's headers (hyper's
    /// timer runs from when it starts waiting for them, so an idle
    /// keep-alive connection counts too).
    header: Duration,
    /// Open connections across every webhook listener, handshakes included.
    connections: Arc<Semaphore>,
}

impl Limits {
    fn new(c: &WebhooksConfig) -> Self {
        Self {
            header: Duration::from_secs(c.header_timeout_secs.max(1)),
            connections: Arc::new(Semaphore::new(c.max_connections.max(1) as usize)),
        }
    }
}

/// Accept connections until `shutdown`, each in its own task holding a
/// connection permit; then wait for the open ones to finish.
async fn accept_loop(
    tcp: TcpListener,
    tls: Option<TlsAcceptor>,
    app: Router,
    limits: Limits,
    shutdown: CancellationToken,
) {
    let mut open = JoinSet::new();
    loop {
        let (stream, peer) = tokio::select! {
            r = tcp.accept() => match r {
                Ok(x) => x,
                Err(e) => {
                    tracing::debug!(error = %e, "webhooks accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            () = shutdown.cancelled() => break,
        };
        while open.try_join_next().is_some() {}
        let Ok(permit) = limits.connections.clone().try_acquire_owned() else {
            refuse_connection(&stream, tls.is_some(), peer);
            continue;
        };
        let _ = stream.set_nodelay(true);
        open.spawn(connection(
            stream,
            peer,
            tls.clone(),
            app.clone(),
            limits.header,
            shutdown.clone(),
            permit,
        ));
    }
    while open.join_next().await.is_some() {}
}

/// A connection over the cap: a 503 without TLS (a proxy in front passes
/// it on), and closed with it, where there is no session to answer in.
fn refuse_connection(stream: &TcpStream, tls: bool, peer: SocketAddr) {
    const BODY: &str = r#"{"error":"too many open connections; try again shortly"}"#;
    if !tls {
        let resp = format!(
            "HTTP/1.1 503 Service Unavailable\r\nretry-after: 5\r\nconnection: close\r\n\
             content-type: application/json\r\ncontent-length: {}\r\n\r\n{BODY}",
            BODY.len()
        );
        // A fresh socket's send buffer takes this whole; if not, it is
        // closed all the same.
        let _ = stream.try_write(resp.as_bytes());
    }
    // One line a minute at most: a flood must not flood the log too.
    static LAST: AtomicI64 = AtomicI64::new(0);
    let now = tern_store::now_ms() / 1000;
    let last = LAST.load(Ordering::Relaxed);
    if now - last >= 60
        && LAST
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        tracing::warn!(%peer, "webhooks listener at max_connections; refusing connections");
    }
}

/// Serve one connection: handshake and headers within `header`, bodies
/// within `body_timeout_secs` (in `deliver`), until it closes or shutdown.
async fn connection(
    stream: TcpStream,
    peer: SocketAddr,
    tls: Option<TlsAcceptor>,
    app: Router,
    header: Duration,
    shutdown: CancellationToken,
    _permit: OwnedSemaphorePermit,
) {
    let io: Box<dyn Io> = match tls {
        None => Box::new(stream),
        Some(acceptor) => match tokio::time::timeout(header, acceptor.accept(stream)).await {
            Ok(Ok(s)) => Box::new(s),
            Ok(Err(e)) => {
                tracing::debug!(%peer, error = %e, "webhooks TLS handshake failed");
                return;
            }
            Err(_) => {
                tracing::debug!(%peer, "webhooks TLS handshake timed out");
                return;
            }
        },
    };
    let svc = TowerToHyperService::new(app.layer(Extension(ConnectInfo(Peer(peer)))));
    let mut builder = http1::Builder::new();
    builder.timer(TokioTimer::new()).header_read_timeout(header);
    let conn = builder.serve_connection(TokioIo::new(io), svc);
    tokio::pin!(conn);
    tokio::select! {
        r = conn.as_mut() => {
            if let Err(e) = r {
                tracing::debug!(%peer, error = %e, "webhooks connection closed");
            }
        }
        () = shutdown.cancelled() => {
            conn.as_mut().graceful_shutdown();
            let _ = conn.await;
        }
    }
}

fn router(state: Arc<Hooks>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "tern webhooks" }))
        .route("/v1/hooks/{id}", post(deliver))
        .fallback(|| async { refuse(StatusCode::NOT_FOUND, "not found") })
        // The handler reads the body itself, up to the live limit.
        .layer(DefaultBodyLimit::disable())
        .with_state(state)
}

fn refuse(status: StatusCode, msg: &str) -> Response {
    let mut resp = (status, axum::Json(json!({ "error": msg }))).into_response();
    if status == StatusCode::TOO_MANY_REQUESTS {
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    }
    resp
}

async fn deliver(
    State(h): State<Arc<Hooks>>,
    ConnectInfo(Peer(peer)): ConnectInfo<Peer>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let ip = auth::client_ip(peer, &headers, h.trust_proxy);
    if h.failures.exceeded(&ip) {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "too many failed deliveries; wait a minute",
        );
    }
    let fail = |status: StatusCode, msg: &str| {
        h.failures.allow(ip);
        refuse(status, msg)
    };
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    if bearer.is_some_and(|t| t.starts_with(auth::TOKEN_PREFIX)) {
        tracing::warn!(target: "audit", %ip, routine = %id, "device token sent to a webhook; refused");
        return fail(
            StatusCode::UNAUTHORIZED,
            "device tokens are never accepted on webhook paths",
        );
    }
    let store = &h.manager.rt.store;
    let found = store.schedule(&id).ok().flatten().and_then(|s| {
        let w = store.webhook(&id).ok().flatten()?;
        let secret = w.secret?;
        (s.trigger() == "webhook").then_some((s, w.auth, secret))
    });
    let Some((routine, kind, secret)) = found else {
        return fail(StatusCode::NOT_FOUND, "no such webhook");
    };
    let live = h.manager.rt.live().webhooks;
    let limit = usize::try_from(live.max_body_kb.max(1).saturating_mul(1024)).unwrap_or(usize::MAX);
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > limit) {
        return refuse(StatusCode::PAYLOAD_TOO_LARGE, "body too large");
    }
    let read = axum::body::to_bytes(body, limit);
    let bytes = match tokio::time::timeout(Duration::from_secs(live.body_timeout_secs.max(1)), read)
        .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => return refuse(StatusCode::PAYLOAD_TOO_LARGE, "body too large"),
        Err(_) => {
            // The rest of the body may still be on its way: close, don't drain.
            let mut resp = fail(StatusCode::REQUEST_TIMEOUT, "the body took too long");
            resp.headers_mut()
                .insert(header::CONNECTION, HeaderValue::from_static("close"));
            return resp;
        }
    };
    let now = tern_store::now_ms() / 1000;
    let signed = match verify(&kind, &secret, &headers, &bytes, now, live.tolerance_secs) {
        Ok(signed) => signed,
        Err(why) => {
            tracing::warn!(target: "audit", %ip, routine = %id, why, "webhook delivery refused");
            return fail(StatusCode::UNAUTHORIZED, why);
        }
    };
    if !h
        .accepted
        .allow_up_to(id.clone(), live.per_minute.max(1) as usize)
    {
        tracing::warn!(routine = %id, "webhook rate limit hit");
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "too many deliveries for this webhook; wait a minute",
        );
    }
    if !routine.enabled {
        return refuse(StatusCode::CONFLICT, "this routine is turned off");
    }
    let event = event(&headers, &bytes);
    match accept(&h.manager, &routine, event, &signed).await {
        Ok(outcome) => {
            let status = match outcome {
                Outcome::Queued => StatusCode::ACCEPTED,
                Outcome::Duplicate | Outcome::Ignored => StatusCode::OK,
            };
            (status, axum::Json(json!({ "status": outcome.as_str() }))).into_response()
        }
        Err(e) => {
            tracing::error!(routine = %id, error = %e, "recording a webhook delivery failed");
            refuse(StatusCode::INTERNAL_SERVER_ERROR, "could not record it")
        }
    }
}

/// Check a delivery against routine's secret. Ok is the material that was
/// signed, which keys the event: the same bytes again are a replay.
fn verify(
    kind: &str,
    secret: &str,
    headers: &HeaderMap,
    body: &[u8],
    now_secs: i64,
    tolerance_secs: u64,
) -> Result<Vec<u8>, &'static str> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    if kind == "bearer" {
        let given = header("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .ok_or("missing Authorization: Bearer <webhook secret>")?;
        // blake3's Hash compares in constant time.
        if blake3::hash(given.as_bytes()) != blake3::hash(secret.as_bytes()) {
            return Err("wrong webhook secret");
        }
        return Ok(body.to_vec());
    }
    let key = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, secret.as_bytes());
    if let Some(sig) = header("x-tern-signature") {
        let mut t = None;
        let mut v1 = None;
        for part in sig.split(',') {
            match part.trim().split_once('=') {
                Some(("t", x)) => t = x.parse::<i64>().ok(),
                Some(("v1", x)) => v1 = unhex(x),
                _ => {}
            }
        }
        let (Some(t), Some(v1)) = (t, v1) else {
            return Err("X-Tern-Signature must be t=<unix seconds>,v1=<hex>");
        };
        let mut signed = format!("{t}.").into_bytes();
        signed.extend_from_slice(body);
        aws_lc_rs::hmac::verify(&key, &signed, &v1).map_err(|_| "bad signature")?;
        if now_secs.abs_diff(t) > tolerance_secs {
            return Err("the signed timestamp is too old (or in the future)");
        }
        return Ok(signed);
    }
    if let Some(sig) = header("x-hub-signature-256") {
        let tag = sig
            .strip_prefix("sha256=")
            .and_then(unhex)
            .ok_or("X-Hub-Signature-256 must be sha256=<hex>")?;
        aws_lc_rs::hmac::verify(&key, body, &tag).map_err(|_| "bad signature")?;
        return Ok(body.to_vec());
    }
    Err("missing signature (X-Tern-Signature or X-Hub-Signature-256)")
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The event a routine (and its gate) sees: the body, as JSON when it
/// parses, and the headers that say what kind of event it is.
pub fn event(headers: &HeaderMap, body: &Bytes) -> Value {
    let mut kept = serde_json::Map::new();
    for (name, value) in headers {
        let name = name.as_str();
        let wanted = KEPT_HEADERS.contains(&name)
            || name.ends_with("-event")
            || name.ends_with("-event-type")
            || name.ends_with("-delivery");
        if wanted && let Ok(v) = value.to_str() {
            kept.insert(name.to_owned(), Value::String(v.to_owned()));
        }
    }
    let body = serde_json::from_slice::<Value>(body)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into_owned()));
    json!({ "headers": kept, "body": body })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Recorded; a run starts for it.
    Queued,
    /// Seen before: a re-delivery or a replay.
    Duplicate,
    /// The gate dropped it.
    Ignored,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Duplicate => "duplicate",
            Self::Ignored => "ignored",
        }
    }
}

/// Take one delivery for routine `s`, whose sender is already checked:
/// run its gate, record the event under a key from `signed`, and start
/// its run if it is new. Shared by the listener and `tern hooks fire`.
pub async fn accept(
    manager: &Arc<Manager>,
    s: &ScheduleRow,
    event: Value,
    signed: &[u8],
) -> anyhow::Result<Outcome> {
    let store = &manager.rt.store;
    let key = format!("h:{}", &blake3::hash(signed).to_hex()[..32]);
    if store.has_routine_event(&s.id, &key)? {
        return Ok(Outcome::Duplicate);
    }
    let (event, check) = match gate(manager, s, &event).await {
        Ok(Some(e)) => (Some(e), "delivery received".to_owned()),
        Ok(None) => (None, "gate: ignored a delivery".to_owned()),
        Err(e) => {
            tracing::warn!(schedule = %s.id, error = %e, "webhook gate failed; running anyway");
            (Some(event), format!("gate failed, ran anyway: {e}"))
        }
    };
    let (payload, state) = match &event {
        Some(e) => (cut(&e.to_string()), "pending"),
        None => (String::new(), "gated"),
    };
    if !store.record_webhook_event(&s.id, &key, &payload, state, &check)? {
        return Ok(Outcome::Duplicate);
    }
    if event.is_none() {
        tracing::info!(schedule = %s.id, "webhook gate dropped a delivery");
        return Ok(Outcome::Ignored);
    }
    tracing::info!(schedule = %s.id, "webhook delivery queued");
    if let Err(e) = super::scheduler::start_waiting_now(manager).await {
        tracing::error!(error = %e, "starting webhook runs failed; the next tick retries");
    }
    Ok(Outcome::Queued)
}

/// The routine's gate on `event`: `None` drops it.
async fn gate(manager: &Manager, s: &ScheduleRow, event: &Value) -> Result<Option<Value>, String> {
    let Some(code) = &s.gate else {
        return Ok(Some(event.clone()));
    };
    let routine = json!({
        "id": s.id,
        "last_run_at": s.last_run_at,
        "now": tern_store::now_ms(),
    });
    let host = manager.rt.lua_host().map_err(|e| e.to_string())?;
    tern_lua::watch::webhook_gate(
        &host,
        &manager.rt.home.connectors_dir(),
        s.gate_connector.as_deref(),
        code,
        &routine,
        event,
    )
    .await
}

/// An event's JSON cut to what a watch event may hold.
fn cut(payload: &str) -> String {
    let max = tern_lua::watch::MAX_PAYLOAD;
    if payload.len() <= max {
        return payload.to_owned();
    }
    format!(
        "{}…[truncated]",
        tern_core::text::prefix_bytes(payload, max)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hmac_hex(secret: &str, msg: &[u8]) -> String {
        let key = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, secret.as_bytes());
        aws_lc_rs::hmac::sign(&key, msg)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn headers(pairs: &[(&'static str, String)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn signatures_are_checked_with_their_timestamp() {
        let (secret, body, now) = ("tern_whk_x", b"{\"a\":1}".as_slice(), 1_700_000_000);
        let mut msg = format!("{now}.").into_bytes();
        msg.extend_from_slice(body);
        let good = headers(&[(
            "x-tern-signature",
            format!("t={now},v1={}", hmac_hex(secret, &msg)),
        )]);
        assert_eq!(verify("hmac", secret, &good, body, now, 300).unwrap(), msg);
        // A replay after the window, the wrong secret, a changed body.
        assert!(verify("hmac", secret, &good, body, now + 301, 300).is_err());
        assert!(verify("hmac", "other", &good, body, now, 300).is_err());
        assert!(verify("hmac", secret, &good, b"{\"a\":2}", now, 300).is_err());
        // GitHub's form signs the body alone.
        let gh = headers(&[(
            "x-hub-signature-256",
            format!("sha256={}", hmac_hex(secret, body)),
        )]);
        assert_eq!(verify("hmac", secret, &gh, body, now, 300).unwrap(), body);
        assert!(verify("hmac", secret, &HeaderMap::new(), body, now, 300).is_err());
        // An hmac hook doesn't take its secret as a bearer token.
        let bearer = headers(&[("authorization", format!("Bearer {secret}"))]);
        assert!(verify("hmac", secret, &bearer, body, now, 300).is_err());
        assert!(verify("bearer", secret, &bearer, body, now, 300).is_ok());
        assert!(verify("bearer", "tern_whk_y", &bearer, body, now, 300).is_err());
        assert!(verify("bearer", secret, &gh, body, now, 300).is_err());
    }

    #[test]
    fn events_keep_the_body_and_only_event_headers() {
        let h = headers(&[
            ("x-github-event", "pull_request".into()),
            ("x-github-delivery", "abc".into()),
            ("x-hub-signature-256", "sha256=00".into()),
            ("authorization", "Bearer x".into()),
            ("cookie", "c=1".into()),
            ("content-type", "application/json".into()),
        ]);
        let e = event(&h, &Bytes::from_static(b"{\"action\":\"opened\"}"));
        assert_eq!(
            e,
            json!({
                "headers": {
                    "x-github-event": "pull_request",
                    "x-github-delivery": "abc",
                    "content-type": "application/json",
                },
                "body": { "action": "opened" },
            })
        );
        let text = event(&HeaderMap::new(), &Bytes::from_static(b"payload=x"));
        assert_eq!(text["body"], "payload=x");
    }

    #[test]
    fn plain_http_only_on_loopback_behind_a_proxy() {
        let mut c = Config::default();
        assert!(check(&c).unwrap().is_empty());
        assert_eq!(base_url(&c), None);
        c.webhooks.listen = vec!["tcp://127.0.0.1:8712".into()];
        assert_eq!(
            url(&c, "sch_1").as_deref(),
            Some("https://127.0.0.1:8712/v1/hooks/sch_1")
        );
        c.webhooks.tls = Some(TlsMode::Off);
        assert!(check(&c).is_err());
        c.api.trust_proxy = true;
        assert!(check(&c).is_ok());
        assert_eq!(
            url(&c, "sch_1").as_deref(),
            Some("http://127.0.0.1:8712/v1/hooks/sch_1")
        );
        c.webhooks.listen = vec!["tcp://0.0.0.0:8712".into()];
        assert!(check(&c).is_err());
        c.webhooks.public_url = Some("https://hooks.example.com/".into());
        assert_eq!(
            url(&c, "sch_1").as_deref(),
            Some("https://hooks.example.com/v1/hooks/sch_1")
        );
        assert!(check_auth("hmac").is_ok() && check_auth("nope").is_err());
    }
}
