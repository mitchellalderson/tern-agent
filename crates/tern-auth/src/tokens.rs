//! OAuth tokens at rest: in secrets.toml (mode 0600, redacted everywhere
//! like any secret) under `conn/<target>/<slug>/oauth/`, and handed out by
//! [`Tokens::access_token`], which refreshes once per connection at a time
//! (providers that rotate refresh tokens revoke the whole grant when an old
//! one is reused).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tern_core::Secrets;

use crate::error::AuthError;
use crate::flow::{Client, OAuthConfig, TokenSet};
use crate::http::Guard;

/// Refresh this long before the server's expiry.
const EARLY_MS: i64 = 60_000;

/// Which connection's tokens: a connector or MCP server and an account.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConnKey {
    pub target: String,
    pub slug: String,
}

impl ConnKey {
    pub fn new(target: &str, slug: &str) -> Self {
        Self {
            target: target.to_owned(),
            slug: slug.to_owned(),
        }
    }

    /// `conn/<target>/<slug>/`: every secret of this connection.
    pub fn prefix(&self) -> String {
        format!("conn/{}/{}/", self.target, self.slug)
    }

    fn oauth(&self, field: &str) -> String {
        format!("{}oauth/{field}", self.prefix())
    }

    /// Where a connector's declared secret `name` lives for this account.
    /// The `default` account keeps plain names, so connectors and MCP
    /// servers set up before connections work unchanged.
    pub fn secret_name(&self, name: &str) -> String {
        if self.slug == crate::DEFAULT_SLUG {
            name.to_owned()
        } else {
            format!("{}{name}", self.prefix())
        }
    }

    /// Where a client secret issued by registration lives.
    pub fn client_secret_name(&self) -> String {
        self.oauth("client_secret")
    }
}

pub struct Tokens {
    secrets: Secrets,
    pub guard: Guard,
    locks: Mutex<HashMap<ConnKey, Arc<tokio::sync::Mutex<()>>>>,
}

impl Tokens {
    pub fn new(secrets: Secrets, guard: Guard) -> Self {
        Self {
            secrets,
            guard,
            locks: Mutex::default(),
        }
    }

    pub fn secrets(&self) -> &Secrets {
        &self.secrets
    }

    fn lock(&self, key: &ConnKey) -> Arc<tokio::sync::Mutex<()>> {
        let mut m = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        m.entry(key.clone()).or_default().clone()
    }

    /// The client for `cfg`, with its secret from secrets.toml.
    pub fn client<'a>(&'a self, key: &ConnKey, cfg: &'a OAuthConfig) -> Client<'a> {
        let secret = cfg
            .client_secret_secret
            .as_deref()
            .and_then(|n| self.secrets.get(n))
            .or_else(|| self.secrets.get(&key.client_secret_name()));
        Client {
            cfg,
            secret,
            guard: &self.guard,
        }
    }

    pub fn save(&self, key: &ConnKey, t: &TokenSet) -> Result<(), AuthError> {
        let expires = t.expires_at.map(|e| e.to_string());
        let mut pairs: Vec<(String, &str)> =
            vec![(key.oauth("access_token"), t.access_token.as_str())];
        if let Some(r) = &t.refresh_token {
            pairs.push((key.oauth("refresh_token"), r));
        }
        if let Some(e) = &expires {
            pairs.push((key.oauth("expires_at"), e));
        }
        let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        self.secrets
            .set_many(&pairs)
            .map_err(|e| AuthError::Invalid(format!("saving tokens: {e}")))?;
        if t.expires_at.is_none() {
            let _ = self.secrets.remove(&key.oauth("expires_at"));
        }
        Ok(())
    }

    pub fn load(&self, key: &ConnKey) -> Option<TokenSet> {
        Some(TokenSet {
            access_token: self.secrets.get(&key.oauth("access_token"))?,
            refresh_token: self.secrets.get(&key.oauth("refresh_token")),
            expires_at: self
                .secrets
                .get(&key.oauth("expires_at"))
                .and_then(|e| e.parse().ok()),
            scope: None,
            id_token: None,
        })
    }

    /// Whether this connection has tokens.
    pub fn has(&self, key: &ConnKey) -> bool {
        self.secrets.get(&key.oauth("access_token")).is_some()
    }

    /// An access token to send. Refreshes when it has expired (or is about
    /// to), or when `rejected` is the token the server just refused with a
    /// 401 and nobody has refreshed it since. `NeedsReauth` means the user
    /// has to sign in again.
    pub async fn access_token(
        &self,
        key: &ConnKey,
        cfg: &OAuthConfig,
        rejected: Option<&str>,
    ) -> Result<String, AuthError> {
        let lock = self.lock(key);
        let _held = lock.lock().await;
        let Some(current) = self.load(key) else {
            return Err(AuthError::NeedsReauth(format!(
                "{} · {} isn't signed in",
                key.target, key.slug
            )));
        };
        let expiring = current
            .expires_at
            .is_some_and(|e| e - EARLY_MS <= tern_store::now_ms());
        let refused = rejected.is_some_and(|r| r == current.access_token);
        if !expiring && !refused {
            return Ok(current.access_token);
        }
        let Some(refresh) = current.refresh_token.as_deref() else {
            return Err(AuthError::NeedsReauth(if refused {
                "the service refused the token and there is no refresh token".into()
            } else {
                "the token expired and there is no refresh token".into()
            }));
        };
        let fresh = self.client(key, cfg).refresh(refresh).await?;
        self.save(key, &fresh)?;
        tracing::info!(target = %key.target, connection = %key.slug, "refreshed OAuth tokens");
        Ok(fresh.access_token)
    }

    /// Revoke at the provider when it has a revocation endpoint (best
    /// effort), then delete every secret of the connection.
    pub async fn forget(&self, key: &ConnKey, cfg: Option<&OAuthConfig>) -> Result<(), AuthError> {
        if let (Some(cfg), Some(t)) = (cfg, self.load(key)) {
            let client = self.client(key, cfg);
            let (token, hint) = match &t.refresh_token {
                Some(r) => (r.clone(), "refresh_token"),
                None => (t.access_token.clone(), "access_token"),
            };
            if let Err(e) = client.revoke(&token, hint).await {
                tracing::warn!(target = %key.target, connection = %key.slug, error = %e, "revoking at the provider failed");
            }
        }
        self.secrets
            .remove_prefix(&key.prefix())
            .map_err(|e| AuthError::Invalid(e.to_string()))?;
        Ok(())
    }
}
