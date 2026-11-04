//! The OAuth flows (tooling spec §6.2): authorization code with PKCE and a
//! loopback redirect (RFC 8252), the device flow (RFC 8628), refresh, and
//! revocation (RFC 7009). Nothing here stores tokens; see `tokens`.

use std::fmt;
use std::time::Duration;

use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::error::{AuthError, short};
use crate::http::Guard;
use crate::pkce;

/// The loopback path the provider redirects to. Anything else gets a 404.
pub const CALLBACK_PATH: &str = "/callback";

/// How tern authenticates itself at the token endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenAuth {
    /// A public client: `client_id` in the body, no secret.
    #[default]
    None,
    /// `client_secret` in the body.
    Post,
    /// HTTP Basic with the client ID and secret.
    Basic,
}

/// Where to sign in and as which client. Stored on the connection; holds
/// no secret (a client secret is named, and lives in secrets.toml).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthConfig {
    /// A built-in profile's id, or a name for a discovered server.
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    pub authorize_url: String,
    pub token_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub userinfo_url: Option<String>,
    pub client_id: String,
    /// The secret holding the client secret, when the provider wants one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret_secret: Option<String>,
    #[serde(default)]
    pub token_auth: TokenAuth,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Extra authorize parameters (`access_type=offline` for Google).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_params: Vec<(String, String)>,
    /// RFC 8707 resource indicator (MCP servers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// The loopback redirect URI a registered client was registered with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_uri: Option<String>,
    /// Where to register again if that port is taken (RFC 7591).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_url: Option<String>,
}

/// Tokens from one grant. `Debug` never shows them.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix milliseconds; `None` when the server didn't say.
    pub expires_at: Option<i64>,
    pub scope: Option<String>,
    pub id_token: Option<String>,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"[REDACTED]")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl TokenSet {
    /// Every token value, for redaction.
    pub fn values(&self) -> Vec<String> {
        let mut v = vec![self.access_token.clone()];
        v.extend(self.refresh_token.clone());
        v.extend(self.id_token.clone());
        v
    }
}

/// One client: the config plus its secret, if any, resolved for a request.
pub struct Client<'a> {
    pub cfg: &'a OAuthConfig,
    pub secret: Option<String>,
    pub guard: &'a Guard,
}

fn parse_url(what: &str, s: &str) -> Result<Url, AuthError> {
    Url::parse(s).map_err(|e| AuthError::Invalid(format!("{what} {s:?} isn't a URL: {e}")))
}

impl Client<'_> {
    /// POST a form to `url` with the client's credentials, and read a
    /// token response or an OAuth error.
    async fn token_request(
        &self,
        url: &str,
        mut form: Vec<(&str, String)>,
    ) -> Result<serde_json::Value, AuthError> {
        let url = parse_url("token endpoint", url)?;
        let client = self.guard.client(&url).await?;
        let mut req = client
            .post(url.clone())
            .header(reqwest::header::ACCEPT, "application/json");
        match (self.cfg.token_auth, &self.secret) {
            (TokenAuth::Basic, Some(secret)) => {
                req = req.basic_auth(form_encode(&self.cfg.client_id), Some(form_encode(secret)));
            }
            (TokenAuth::Post, Some(secret)) => {
                form.push(("client_id", self.cfg.client_id.clone()));
                form.push(("client_secret", secret.clone()));
            }
            _ => form.push(("client_id", self.cfg.client_id.clone())),
        }
        let mut redact: Vec<String> = form
            .iter()
            .filter(|(k, _)| {
                matches!(
                    *k,
                    "code"
                        | "code_verifier"
                        | "refresh_token"
                        | "client_secret"
                        | "device_code"
                        | "token"
                )
            })
            .map(|(_, v)| v.clone())
            .collect();
        redact.extend(self.secret.clone());
        let (status, _, body) = self
            .guard
            .send(
                &crate::http::origin(&url),
                req.header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(encode_form(&form)),
            )
            .await
            .map_err(|e| scrub_err(e, &redact))?;
        let json: serde_json::Value = match serde_json::from_str(&body) {
            Ok(j) => j,
            // GitHub answers in form encoding without `Accept`; some
            // servers do anyway.
            Err(_) => form_to_json(&body),
        };
        if let Some(error) = json.get("error").and_then(|e| e.as_str()) {
            let description = json
                .get("error_description")
                .and_then(|d| d.as_str())
                .map(|d| tern_core::secrets::redact_with(&redact, &short(d, 200)));
            return Err(AuthError::OAuth {
                error: short(error, 60),
                description,
            });
        }
        if !(200..300).contains(&status) {
            return Err(AuthError::Http(format!(
                "{}: HTTP {status}",
                crate::http::origin(&url)
            )));
        }
        Ok(json)
    }

    /// Exchange an authorization code.
    pub async fn exchange(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenSet, AuthError> {
        let mut form = vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", code.to_owned()),
            ("redirect_uri", redirect_uri.to_owned()),
            ("code_verifier", verifier.to_owned()),
        ];
        if let Some(r) = &self.cfg.resource {
            form.push(("resource", r.clone()));
        }
        let json = self.token_request(&self.cfg.token_url, form).await?;
        token_set(&json, None)
    }

    /// Trade a refresh token for new tokens. A rejected grant means the
    /// user has to sign in again. A response without a refresh token keeps
    /// the old one.
    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenSet, AuthError> {
        let mut form = vec![
            ("grant_type", "refresh_token".to_owned()),
            ("refresh_token", refresh_token.to_owned()),
        ];
        if let Some(r) = &self.cfg.resource {
            form.push(("resource", r.clone()));
        }
        match self.token_request(&self.cfg.token_url, form).await {
            Ok(json) => token_set(&json, Some(refresh_token)),
            Err(AuthError::OAuth { error, description })
                if matches!(
                    error.as_str(),
                    "invalid_grant" | "unauthorized_client" | "invalid_client"
                ) =>
            {
                Err(AuthError::NeedsReauth(match description {
                    Some(d) => format!("{error}: {d}"),
                    None => error,
                }))
            }
            Err(e) => Err(e),
        }
    }

    /// Revoke a token at the provider (best effort; RFC 7009).
    pub async fn revoke(&self, token: &str, hint: &str) -> Result<(), AuthError> {
        let Some(url) = &self.cfg.revocation_url else {
            return Ok(());
        };
        self.token_request(
            url,
            vec![
                ("token", token.to_owned()),
                ("token_type_hint", hint.to_owned()),
            ],
        )
        .await
        .map(|_| ())
    }

    /// Start the device flow (RFC 8628).
    pub async fn device_start(&self) -> Result<DeviceCode, AuthError> {
        let url = self.cfg.device_url.as_deref().ok_or_else(|| {
            AuthError::Invalid(format!("{} has no device flow", self.cfg.provider))
        })?;
        let mut form = vec![("scope", self.cfg.scopes.join(" "))];
        if let Some(r) = &self.cfg.resource {
            form.push(("resource", r.clone()));
        }
        let json = self.token_request(url, form).await?;
        let s = |k: &str| json.get(k).and_then(|v| v.as_str()).map(str::to_owned);
        let device_code = s("device_code")
            .ok_or_else(|| AuthError::Invalid("the device endpoint sent no device_code".into()))?;
        let user_code = s("user_code")
            .ok_or_else(|| AuthError::Invalid("the device endpoint sent no user_code".into()))?;
        // Google calls it `verification_url`.
        let verification_uri = s("verification_uri")
            .or_else(|| s("verification_url"))
            .ok_or_else(|| {
                AuthError::Invalid("the device endpoint sent no verification_uri".into())
            })?;
        let n = |k: &str| json.get(k).and_then(serde_json::Value::as_u64);
        Ok(DeviceCode {
            device_code,
            user_code: short(&user_code, 40),
            verification_uri: short(&verification_uri, 300),
            verification_uri_complete: s("verification_uri_complete").map(|u| short(&u, 400)),
            interval: Duration::from_secs(n("interval").unwrap_or(5).clamp(1, 60)),
            expires_in: Duration::from_secs(n("expires_in").unwrap_or(900).clamp(30, 3600)),
        })
    }

    /// Poll until the user finishes the device flow, it expires, or
    /// `deadline` passes.
    pub async fn device_poll(
        &self,
        code: &DeviceCode,
        deadline: tokio::time::Instant,
    ) -> Result<TokenSet, AuthError> {
        let deadline = deadline.min(tokio::time::Instant::now() + code.expires_in);
        let mut interval = code.interval;
        loop {
            if tokio::time::Instant::now() + interval > deadline {
                return Err(AuthError::TimedOut);
            }
            tokio::time::sleep(interval).await;
            let form = vec![
                (
                    "grant_type",
                    "urn:ietf:params:oauth:grant-type:device_code".to_owned(),
                ),
                ("device_code", code.device_code.clone()),
            ];
            match self.token_request(&self.cfg.token_url, form).await {
                Ok(json) => return token_set(&json, None),
                Err(AuthError::OAuth { error, description }) => match error.as_str() {
                    "authorization_pending" => {}
                    "slow_down" => interval += Duration::from_secs(5),
                    "access_denied" => return Err(AuthError::Denied),
                    "expired_token" => return Err(AuthError::TimedOut),
                    _ => return Err(AuthError::OAuth { error, description }),
                },
                Err(e) => return Err(e),
            }
        }
    }

    /// Who signed in, for the connection's label: the userinfo endpoint's
    /// `email`, `preferred_username`, `login` or `name`, else the ID
    /// token's `email`. Only ever a label; never trusted for access.
    pub async fn account(&self, tokens: &TokenSet) -> Option<String> {
        if let Some(url) = &self.cfg.userinfo_url
            && let Ok(url) = Url::parse(url)
            && let Ok(client) = self.guard.client(&url).await
            && let Ok((200, _, body)) = self
                .guard
                .send(
                    "userinfo",
                    client
                        .get(url)
                        .bearer_auth(&tokens.access_token)
                        .header(reqwest::header::ACCEPT, "application/json"),
                )
                .await
            && let Ok(j) = serde_json::from_str::<serde_json::Value>(&body)
            && let Some(a) = account_of(&j)
        {
            return Some(a);
        }
        let claims = tokens.id_token.as_deref()?.split('.').nth(1)?;
        let j: serde_json::Value = serde_json::from_slice(&crate::b64::decode(claims)?).ok()?;
        account_of(&j)
    }
}

fn account_of(j: &serde_json::Value) -> Option<String> {
    ["email", "preferred_username", "login", "upn", "name"]
        .iter()
        .find_map(|k| j.get(*k).and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
        .map(|s| short(s, 80))
}

/// A device-flow sign-in in progress: show `user_code` and
/// `verification_uri` to the user.
#[derive(Clone)]
pub struct DeviceCode {
    device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub interval: Duration,
    pub expires_in: Duration,
}

impl fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceCode")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .finish_non_exhaustive()
    }
}

fn token_set(json: &serde_json::Value, old_refresh: Option<&str>) -> Result<TokenSet, AuthError> {
    let s = |k: &str| {
        json.get(k)
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let access_token = s("access_token")
        .ok_or_else(|| AuthError::Invalid("the token response has no access_token".into()))?;
    if let Some(t) = s("token_type")
        && !t.eq_ignore_ascii_case("bearer")
    {
        return Err(AuthError::Invalid(format!(
            "token type {:?} isn't supported (only Bearer)",
            short(&t, 20)
        )));
    }
    let expires_in = json.get("expires_in").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    });
    Ok(TokenSet {
        access_token,
        refresh_token: s("refresh_token").or_else(|| old_refresh.map(str::to_owned)),
        expires_at: expires_in
            .map(|secs| tern_store::now_ms() + secs.clamp(0, 10 * 365 * 86_400) * 1000),
        scope: s("scope"),
        id_token: s("id_token"),
    })
}

fn form_to_json(body: &str) -> serde_json::Value {
    let pairs = url_pairs(body);
    serde_json::Value::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (k, serde_json::Value::String(v)))
            .collect(),
    )
}

fn url_pairs(q: &str) -> Vec<(String, String)> {
    Url::parse(&format!("http://x/?{q}"))
        .map(|u| u.query_pairs().into_owned().collect())
        .unwrap_or_default()
}

/// `application/x-www-form-urlencoded`.
fn encode_form(pairs: &[(&str, String)]) -> String {
    let mut u = Url::parse("http://x/").unwrap_or_else(|_| unreachable!());
    u.query_pairs_mut()
        .extend_pairs(pairs.iter().map(|(k, v)| (*k, v.as_str())));
    u.query().unwrap_or_default().to_owned()
}

/// RFC 6749 §2.3.1: client credentials are form-encoded before Basic.
fn form_encode(s: &str) -> String {
    let mut u = Url::parse("http://x/").unwrap_or_else(|_| unreachable!());
    u.query_pairs_mut().append_pair("v", s);
    u.query()
        .unwrap_or_default()
        .trim_start_matches("v=")
        .to_owned()
}

fn scrub_err(e: AuthError, values: &[String]) -> AuthError {
    let mut values: Vec<String> = values.iter().filter(|v| v.len() >= 4).cloned().collect();
    values.sort_by_key(|v| std::cmp::Reverse(v.len()));
    match e {
        AuthError::Http(m) => AuthError::Http(tern_core::secrets::redact_with(&values, &m)),
        other => other,
    }
}

/// An authorization-code sign-in waiting for the browser: open
/// [`PendingSignIn::url`], then [`PendingSignIn::finish`].
pub struct PendingSignIn {
    url: String,
    state: String,
    verifier: String,
    redirect_uri: String,
    listener: TcpListener,
    issuer: Option<String>,
}

impl fmt::Debug for PendingSignIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingSignIn")
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

/// Bind a one-shot loopback listener on `127.0.0.1` (on `port`, or any
/// free one) and build the authorize URL with PKCE and a fresh `state`.
pub async fn begin(
    cfg: &OAuthConfig,
    guard: &Guard,
    port: Option<u16>,
) -> Result<PendingSignIn, AuthError> {
    let (listener, redirect_uri) = bind(port).await?;
    begin_on(cfg, guard, listener, redirect_uri)
}

/// The loopback listener and its redirect URI, before a client exists
/// (registration needs the URI first).
pub async fn bind(port: Option<u16>) -> Result<(TcpListener, String), AuthError> {
    let listener = TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))
        .await
        .map_err(|e| AuthError::Http(format!("opening the sign-in listener: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| AuthError::Http(e.to_string()))?
        .port();
    Ok((listener, format!("http://127.0.0.1:{port}{CALLBACK_PATH}")))
}

/// [`begin`] on a listener from [`bind`].
pub fn begin_on(
    cfg: &OAuthConfig,
    guard: &Guard,
    listener: TcpListener,
    redirect_uri: String,
) -> Result<PendingSignIn, AuthError> {
    let mut url = parse_url("authorize endpoint", &cfg.authorize_url)?;
    if url.scheme() != "https" && !guard.is_local(&url) {
        return Err(AuthError::Refused(format!(
            "the authorize endpoint {} isn't https",
            crate::http::origin(&url)
        )));
    }
    let (state, verifier) = (pkce::random(24), pkce::verifier());
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &cfg.client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("state", &state)
            .append_pair("code_challenge", &pkce::challenge(&verifier))
            .append_pair("code_challenge_method", "S256");
        if !cfg.scopes.is_empty() {
            q.append_pair("scope", &cfg.scopes.join(" "));
        }
        if let Some(r) = &cfg.resource {
            q.append_pair("resource", r);
        }
        for (k, v) in &cfg.extra_params {
            q.append_pair(k, v);
        }
    }
    Ok(PendingSignIn {
        url: url.into(),
        state,
        verifier,
        redirect_uri,
        listener,
        issuer: cfg.issuer.clone(),
    })
}

impl PendingSignIn {
    /// The provider's sign-in page, for the user's browser.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Wait for the browser to come back with a code, then exchange it.
    /// Requests for another path, from another `Host`, or with the wrong
    /// `state` are answered with an error page and ignored; the listener
    /// takes the first valid one and closes.
    pub async fn finish(self, client: &Client<'_>) -> Result<TokenSet, AuthError> {
        let code = self.wait_for_code().await?;
        client
            .exchange(&code, &self.verifier, &self.redirect_uri)
            .await
    }

    async fn wait_for_code(&self) -> Result<String, AuthError> {
        let host = self
            .redirect_uri
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        loop {
            let (stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|e| AuthError::Http(format!("sign-in listener: {e}")))?;
            let handled =
                tokio::time::timeout(Duration::from_secs(5), self.handle(stream, &host)).await;
            if let Ok(Some(result)) = handled {
                return result;
            }
        }
    }

    /// One request to the listener: `Some` ends the wait.
    async fn handle(&self, mut stream: TcpStream, host: &str) -> Option<Result<String, AuthError>> {
        let mut buf = vec![0u8; 8192];
        let mut n = 0;
        while n < buf.len() {
            let read = stream.read(&mut buf[n..]).await.ok()?;
            if read == 0 {
                break;
            }
            n += read;
            if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&buf[..n]).into_owned();
        let mut lines = head.split("\r\n");
        let mut first = lines.next().unwrap_or_default().split(' ');
        let (method, target) = (
            first.next().unwrap_or_default(),
            first.next().unwrap_or_default(),
        );
        // A page in the browser can reach 127.0.0.1 through a DNS name it
        // controls; it can't make the browser send our Host header.
        let host_ok = lines.any(|l| {
            l.split_once(':')
                .is_some_and(|(k, v)| k.trim().eq_ignore_ascii_case("host") && v.trim() == host)
        });
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        if method != "GET" || path != CALLBACK_PATH || !host_ok {
            reply(&mut stream, 404, "Not found.").await;
            return None;
        }
        let params = url_pairs(query);
        let get = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        if !get("state").is_some_and(|s| pkce::same(s, &self.state)) {
            reply(
                &mut stream,
                400,
                "This sign-in link doesn't match the one tern started. You can close this tab.",
            )
            .await;
            return None;
        }
        // RFC 9207: a server that names itself must be the one we asked.
        if let (Some(iss), Some(expected)) = (get("iss"), &self.issuer)
            && iss.trim_end_matches('/') != expected.trim_end_matches('/')
        {
            reply(
                &mut stream,
                400,
                "The sign-in came back from a different server. You can close this tab.",
            )
            .await;
            return Some(Err(AuthError::Invalid(format!(
                "the sign-in came back from {:?}, not {expected:?}",
                short(iss, 80)
            ))));
        }
        if let Some(error) = get("error") {
            reply(
                &mut stream,
                200,
                "The sign-in was not completed. You can close this tab and go back to tern.",
            )
            .await;
            return Some(Err(if error == "access_denied" {
                AuthError::Denied
            } else {
                AuthError::OAuth {
                    error: short(error, 60),
                    description: get("error_description").map(|d| short(d, 200)),
                }
            }));
        }
        let Some(code) = get("code").filter(|c| !c.is_empty()) else {
            reply(
                &mut stream,
                400,
                "The sign-in came back without a code. You can close this tab.",
            )
            .await;
            return None;
        };
        reply(
            &mut stream,
            200,
            "Signed in. You can close this tab and go back to tern.",
        )
        .await;
        Some(Ok(code.to_owned()))
    }
}

/// A fixed page; nothing from the request is echoed into it.
async fn reply(stream: &mut TcpStream, status: u16, text: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>tern</title><body style=\"font:16px system-ui;margin:3em\"><p>{text}</p>"
    );
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_sets_never_show_in_debug() {
        let t = TokenSet {
            access_token: "at-secret-1".into(),
            refresh_token: Some("rt-secret-2".into()),
            expires_at: None,
            scope: None,
            id_token: None,
        };
        let d = format!("{t:?}");
        assert!(!d.contains("secret"), "{d}");
    }

    #[test]
    fn token_responses() {
        let j = serde_json::json!({"access_token": "a", "token_type": "Bearer", "expires_in": 60});
        let t = token_set(&j, Some("old")).unwrap();
        assert_eq!(
            t.refresh_token.as_deref(),
            Some("old"),
            "kept when not rotated"
        );
        assert!(t.expires_at.unwrap() > tern_store::now_ms());
        assert!(token_set(&serde_json::json!({"token_type": "Bearer"}), None).is_err());
        assert!(
            token_set(
                &serde_json::json!({"access_token": "a", "token_type": "mac"}),
                None
            )
            .is_err()
        );
        let gh = form_to_json("access_token=gho_x&scope=repo&token_type=bearer");
        assert_eq!(token_set(&gh, None).unwrap().access_token, "gho_x");
        assert_eq!(form_encode("a b:c"), "a+b%3Ac");
    }
}
