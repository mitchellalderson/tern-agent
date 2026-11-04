//! Getting a client ID with no setup: dynamic client registration
//! (RFC 7591) or a client ID metadata document.

use reqwest::Url;
use serde_json::{Value, json};

use crate::error::{AuthError, short};
use crate::flow::TokenAuth;
use crate::http::Guard;

/// What registration gave back.
#[derive(Clone)]
pub struct Registered {
    pub client_id: String,
    /// Some servers issue one even to a public client.
    pub client_secret: Option<String>,
    pub token_auth: TokenAuth,
}

impl std::fmt::Debug for Registered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registered")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

/// Register tern as a public, native client with the exact loopback
/// `redirect_uri` the sign-in will use.
pub async fn register(
    guard: &Guard,
    endpoint: &str,
    redirect_uri: &str,
    scopes: &[String],
) -> Result<Registered, AuthError> {
    let url = Url::parse(endpoint)
        .map_err(|e| AuthError::Invalid(format!("registration endpoint {endpoint:?}: {e}")))?;
    let client = guard.client(&url).await?;
    let mut body = json!({
        "client_name": "tern",
        "client_uri": "https://github.com/inngest/tern",
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "application_type": "native",
    });
    if !scopes.is_empty() {
        body["scope"] = Value::String(scopes.join(" "));
    }
    let (status, _, text) = guard
        .send("client registration", client.post(url).json(&body))
        .await?;
    let j: Value = serde_json::from_str(&text).unwrap_or_default();
    if !(200..300).contains(&status) {
        let error = j
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("registration failed");
        return Err(AuthError::OAuth {
            error: short(error, 60),
            description: j
                .get("error_description")
                .and_then(Value::as_str)
                .map(|d| short(d, 200)),
        });
    }
    let client_id = j
        .get("client_id")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| AuthError::Invalid("registration returned no client_id".into()))?;
    let client_secret = j
        .get("client_secret")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let token_auth = match j.get("token_endpoint_auth_method").and_then(Value::as_str) {
        Some("client_secret_basic") if client_secret.is_some() => TokenAuth::Basic,
        Some("client_secret_post") if client_secret.is_some() => TokenAuth::Post,
        _ => TokenAuth::None,
    };
    Ok(Registered {
        client_id: short(client_id, 300),
        client_secret,
        token_auth,
    })
}
