//! Connecting an account, shared by `connection_add`, the CLI and the API
//! (tooling spec §6.7.4): get a client (configured, registered or a
//! metadata document), run the browser leg or the device flow, learn the
//! account, and record the connection. The caller presents what the user
//! has to open and passes back their answer; the model is never involved.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::connections::{Conn, ConnAuth, Connections, Kind};
use crate::error::AuthError;
use crate::flow::{self, OAuthConfig, TokenSet};
use crate::tokens::ConnKey;

/// What the user has to do, for a card or the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shown {
    /// Open this page in a browser on this machine.
    Url { url: String, device_available: bool },
    /// Enter `user_code` at `verification_uri` (any device).
    Device {
        user_code: String,
        verification_uri: String,
        complete: Option<String>,
    },
}

/// The user's answer to what was shown: `None` when nobody can answer
/// (the presenter gave up), `Some("cancel")`, or `Some("device")` to
/// switch to a code.
pub type Answer = Pin<Box<dyn Future<Output = Option<String>> + Send>>;

/// An answer that never comes (the CLI: Ctrl-C ends it).
pub fn never() -> Answer {
    Box::pin(std::future::pending())
}

/// How to get a client ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientSource {
    /// `cfg.client_id` is set (configured or registered before).
    Ready,
    /// Register at this endpoint (RFC 7591).
    Register(String),
}

/// A finished sign-in, not yet recorded.
pub struct SignedIn {
    pub tokens: TokenSet,
    pub account: Option<String>,
    /// The config as used: the client ID registration gave, the redirect
    /// URI it was registered with.
    pub cfg: OAuthConfig,
    /// A secret registration issued.
    pub client_secret: Option<String>,
}

impl std::fmt::Debug for SignedIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignedIn")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

/// Run an OAuth sign-in. `present` is called with each thing to show (the
/// page, then perhaps a device code) and returns the user's answer.
pub async fn oauth(
    conns: &Connections,
    mut cfg: OAuthConfig,
    client: ClientSource,
    prefer_device: bool,
    timeout: Duration,
    mut present: impl FnMut(Shown) -> Answer,
) -> Result<SignedIn, AuthError> {
    let guard = &conns.tokens.guard;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut client_secret = None;
    let device_ok = cfg.device_url.is_some() && !cfg.client_id.is_empty();
    if !(prefer_device && device_ok) {
        // The loopback leg. A registered client must come back to the port
        // it was registered with; if that port is taken, register again.
        let wanted = cfg
            .redirect_uri
            .as_deref()
            .and_then(|u| reqwest::Url::parse(u).ok())
            .and_then(|u| u.port());
        let (listener, redirect) = match (wanted, &client) {
            (Some(p), ClientSource::Ready) => match flow::bind(Some(p)).await {
                Ok(b) => b,
                Err(e) if cfg.client_id.is_empty() => return Err(e),
                Err(_) => flow::bind(None).await?,
            },
            _ => flow::bind(None).await?,
        };
        let must_register = match &client {
            ClientSource::Register(_) => true,
            ClientSource::Ready => cfg.redirect_uri.as_deref().is_some_and(|r| r != redirect),
        };
        if must_register {
            let ClientSource::Register(endpoint) = &client else {
                return Err(AuthError::Invalid(
                    "the registered redirect port is taken and the server has no registration endpoint; free the port or connect again later".into(),
                ));
            };
            let reg = crate::register::register(guard, endpoint, &redirect, &cfg.scopes).await?;
            cfg.client_id = reg.client_id;
            cfg.token_auth = reg.token_auth;
            cfg.redirect_uri = Some(redirect.clone());
            client_secret = reg.client_secret;
        }
        if cfg.client_id.is_empty() {
            return Err(AuthError::Invalid(format!(
                "no OAuth client ID for {}: register an app and add it under [auth.clients.{}]",
                cfg.provider, cfg.provider
            )));
        }
        let pending = flow::begin_on(&cfg, guard, listener, redirect)?;
        let answer = present(Shown::Url {
            url: pending.url().to_owned(),
            device_available: cfg.device_url.is_some(),
        });
        let c = client_for(conns, &cfg, client_secret.clone());
        let c = crate::flow::Client {
            cfg: &cfg,
            secret: c,
            guard,
        };
        let outcome = tokio::select! {
            r = pending.finish(&c) => Some(r),
            a = answer => match a.as_deref() {
                Some("device") if cfg.device_url.is_some() => None,
                _ => return Err(AuthError::Cancelled),
            },
            () = tokio::time::sleep_until(deadline) => return Err(AuthError::TimedOut),
        };
        if let Some(r) = outcome {
            return finish(conns, cfg, client_secret, r?).await;
        }
    }
    // The device flow.
    let secret = client_for(conns, &cfg, client_secret.clone());
    let c = crate::flow::Client {
        cfg: &cfg,
        secret,
        guard,
    };
    let code = c.device_start().await?;
    let answer = present(Shown::Device {
        user_code: code.user_code.clone(),
        verification_uri: code.verification_uri.clone(),
        complete: code.verification_uri_complete.clone(),
    });
    let tokens = tokio::select! {
        r = c.device_poll(&code, deadline) => r?,
        a = answer => {
            let _ = a;
            return Err(AuthError::Cancelled);
        }
    };
    finish(conns, cfg, client_secret, tokens).await
}

fn client_for(
    conns: &Connections,
    cfg: &OAuthConfig,
    registered: Option<String>,
) -> Option<String> {
    registered.or_else(|| {
        cfg.client_secret_secret
            .as_deref()
            .and_then(|n| conns.tokens.secrets().get(n))
    })
}

async fn finish(
    conns: &Connections,
    cfg: OAuthConfig,
    client_secret: Option<String>,
    tokens: TokenSet,
) -> Result<SignedIn, AuthError> {
    let secret = client_for(conns, &cfg, client_secret.clone());
    let c = crate::flow::Client {
        cfg: &cfg,
        secret,
        guard: &conns.tokens.guard,
    };
    let account = c.account(&tokens).await;
    Ok(SignedIn {
        tokens,
        account,
        cfg,
        client_secret,
    })
}

/// How a sign-in was recorded.
#[derive(Debug, Clone, PartialEq)]
pub enum Recorded {
    Connected(Conn),
    /// The account was already connected: its connection was refreshed
    /// instead of making a duplicate (§6.8.3).
    Reconnected(Conn),
}

impl Recorded {
    pub fn conn(&self) -> &Conn {
        match self {
            Recorded::Connected(c) | Recorded::Reconnected(c) => c,
        }
    }
}

/// Record a finished OAuth sign-in as connection `slug` of `target` (or a
/// slug made from the account), or as the existing connection with the
/// same account.
pub fn record_oauth(
    conns: &Connections,
    kind: Kind,
    target: &str,
    slug: Option<&str>,
    label: Option<&str>,
    signed: SignedIn,
) -> Result<Recorded, String> {
    let existing = slug.and_then(|s| conns.get(kind, target, s)).or_else(|| {
        signed
            .account
            .as_deref()
            .and_then(|a| conns.with_account(kind, target, a))
    });
    let auth = ConnAuth::OAuth(Box::new(signed.cfg.clone()));
    let save = |key: &ConnKey| -> Result<(), String> {
        conns
            .tokens
            .save(key, &signed.tokens)
            .map_err(|e| e.to_string())?;
        if let Some(s) = &signed.client_secret {
            conns
                .tokens
                .secrets()
                .set(&key.client_secret_name(), s)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    if let Some(c) = existing {
        save(&c.key())?;
        conns.signed_in(&c, signed.account.as_deref(), Some(&auth))?;
        let c = conns.get(kind, target, &c.slug).unwrap_or(c);
        return Ok(Recorded::Reconnected(c));
    }
    let slug = match slug {
        Some(s) => s.to_owned(),
        None => new_slug(conns, kind, target, signed.account.as_deref()),
    };
    conns.check_new(kind, target, &slug)?;
    // Tokens first: a row without them would be a connection that can't
    // sign in.
    save(&ConnKey::new(target, &slug))?;
    let c = conns.add(kind, target, &slug, label, signed.account.as_deref(), &auth)?;
    Ok(Recorded::Connected(c))
}

/// Record a key-based connection whose secrets were just stored.
pub fn record_key(
    conns: &Connections,
    kind: Kind,
    target: &str,
    slug: &str,
    label: Option<&str>,
    names: Vec<String>,
) -> Result<Recorded, String> {
    let auth = ConnAuth::Key { names };
    if let Some(c) = conns.get(kind, target, slug) {
        conns.signed_in(&c, None, Some(&auth))?;
        return Ok(Recorded::Reconnected(
            conns.get(kind, target, slug).unwrap_or(c),
        ));
    }
    conns
        .add(kind, target, slug, label, None, &auth)
        .map(Recorded::Connected)
}

/// Mail providers whose domain says nothing about the account.
const PERSONAL_MAIL: &[&str] = &[
    "gmail",
    "googlemail",
    "outlook",
    "hotmail",
    "live",
    "icloud",
    "me",
    "yahoo",
    "proton",
    "protonmail",
    "fastmail",
    "hey",
    "aol",
];

/// A slug for a new connection: `default` for the first; else from the
/// account (`mitch@inngest.com` → `inngest`, `mitch@gmail.com` → `mitch`),
/// made unique.
pub fn new_slug(conns: &Connections, kind: Kind, target: &str, account: Option<&str>) -> String {
    let taken: Vec<String> = conns
        .list(kind, target)
        .into_iter()
        .map(|c| c.slug)
        .collect();
    if taken.is_empty() {
        return crate::DEFAULT_SLUG.into();
    }
    let base = account
        .map(|a| match a.split_once('@') {
            Some((local, domain)) => {
                let d = domain
                    .split('.')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if PERSONAL_MAIL.contains(&d.as_str()) || d.is_empty() {
                    local.to_owned()
                } else {
                    d
                }
            }
            None => a.to_owned(),
        })
        .map(|s| {
            s.to_ascii_lowercase()
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '-'
                    }
                })
                .take(13)
                .collect::<String>()
                .trim_matches('-')
                .to_owned()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "account".into());
    if !taken.contains(&base) {
        return base;
    }
    (2..100)
        .map(|n| format!("{base}{n}"))
        .find(|s| !taken.contains(s))
        .unwrap_or_else(|| format!("a{}", tern_store::now_ms() % 100_000))
}
