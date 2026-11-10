//! Account sign-in on a server core (deployment spec §10.4a).
//!
//! A TCP request may carry a Clerk-issued JWT instead of a device token. It
//! is accepted when the signature checks out against the issuer's JWKS, the
//! issuer, audience and expiry match, and `sub` is the core's `account.owner`.
//! A valid token for another account is refused. Nothing here runs unless
//! `[account] issuer` is set, and the Unix socket never sees it.

use std::time::{Duration, Instant};

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use tern_core::config::AccountConfig;
use tokio::sync::Mutex;

/// How long a fetched key set is trusted.
const JWKS_TTL: Duration = Duration::from_secs(60 * 60);
/// Fewest seconds between fetches, so unknown key ids can't hammer the issuer.
const JWKS_MIN_GAP: Duration = Duration::from_secs(30);
const LEEWAY_SECS: u64 = 30;

/// The claims a tern account is made of.
#[derive(Debug, Clone, Deserialize)]
pub struct Claims {
    /// The Clerk user id (`user_…`).
    pub sub: String,
    #[serde(default)]
    pub email: Option<String>,
    pub exp: u64,
}

#[derive(Debug)]
pub enum Verdict {
    /// A valid token for the account that owns this core.
    Owner(Claims),
    /// A valid token for some other account.
    Stranger(Claims),
    /// Not acceptable: bad signature, expired, wrong issuer or audience.
    Rejected(String),
}

struct Cache {
    keys: Option<JwkSet>,
    fetched: Option<Instant>,
}

pub struct AccountVerifier {
    issuer: String,
    audience: String,
    owner: Option<String>,
    jwks_url: String,
    http: reqwest::Client,
    cache: Mutex<Cache>,
}

impl AccountVerifier {
    /// `None` when accounts are off or misconfigured (logged).
    pub fn from_config(cfg: &AccountConfig) -> Option<Self> {
        if !cfg.enabled() {
            return None;
        }
        if let Err(e) = cfg.check() {
            tracing::error!("accounts disabled: {e}");
            return None;
        }
        Some(Self {
            issuer: cfg.issuer.clone()?,
            audience: cfg.audience.clone()?,
            owner: cfg.owner.clone().filter(|o| !o.is_empty()),
            jwks_url: cfg.jwks_url()?,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .ok()?,
            cache: Mutex::new(Cache {
                keys: None,
                fetched: None,
            }),
        })
    }

    /// Whether `token` has the shape of a JWT (three dot-separated parts).
    pub fn looks_like_jwt(token: &str) -> bool {
        token.split('.').count() == 3
    }

    pub async fn verify(&self, token: &str) -> Verdict {
        match self.verify_inner(token).await {
            Ok(claims) if self.owner.as_deref() == Some(claims.sub.as_str()) => {
                Verdict::Owner(claims)
            }
            Ok(claims) => Verdict::Stranger(claims),
            Err(e) => Verdict::Rejected(e),
        }
    }

    async fn verify_inner(&self, token: &str) -> Result<Claims, String> {
        let header = decode_header(token).map_err(|_| "not a valid account token".to_owned())?;
        // Only asymmetric algorithms: never HS* (key confusion) or none.
        if !matches!(header.alg, Algorithm::RS256 | Algorithm::ES256) {
            return Err("unsupported token algorithm".into());
        }
        let kid = header.kid.ok_or("token has no key id")?;
        let key = self.key(&kid).await?;
        let mut v = Validation::new(header.alg);
        v.set_issuer(&[&self.issuer]);
        v.set_audience(&[&self.audience]);
        v.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        v.leeway = LEEWAY_SECS;
        decode::<Claims>(token, &key, &v)
            .map(|d| d.claims)
            .map_err(|e| format!("account token refused: {}", reason(e.kind())))
    }

    async fn key(&self, kid: &str) -> Result<DecodingKey, String> {
        let mut cache = self.cache.lock().await;
        let stale = cache.fetched.is_none_or(|t| t.elapsed() > JWKS_TTL);
        let missing = cache.keys.as_ref().is_none_or(|k| k.find(kid).is_none());
        let may_fetch = cache.fetched.is_none_or(|t| t.elapsed() > JWKS_MIN_GAP);
        if (stale || missing) && may_fetch {
            cache.fetched = Some(Instant::now());
            match self.fetch().await {
                Ok(keys) => cache.keys = Some(keys),
                Err(e) => {
                    tracing::warn!("fetching account keys failed: {e}");
                    if cache.keys.is_none() {
                        return Err("account keys unavailable".into());
                    }
                }
            }
        }
        let jwk = cache
            .keys
            .as_ref()
            .and_then(|k| k.find(kid))
            .ok_or("token signed by an unknown key")?;
        DecodingKey::from_jwk(jwk).map_err(|_| "unusable signing key".to_owned())
    }

    /// Fetch the key set now; used by `tern account status` as a check.
    pub async fn fetch(&self) -> Result<JwkSet, String> {
        let resp = self
            .http
            .get(&self.jwks_url)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("{} answered {}", self.jwks_url, resp.status()));
        }
        resp.json().await.map_err(|e| e.to_string())
    }
}

fn reason(kind: &jsonwebtoken::errors::ErrorKind) -> &'static str {
    use jsonwebtoken::errors::ErrorKind as K;
    match kind {
        K::ExpiredSignature => "expired",
        K::InvalidIssuer => "wrong issuer",
        K::InvalidAudience => "wrong audience",
        K::InvalidSignature => "bad signature",
        _ => "invalid",
    }
}
