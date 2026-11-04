//! `auth_discover` (tooling spec §6.7.1): how a service wants to be signed
//! into, found by host code in a fixed order (built-in profiles, the MCP
//! authorization flow's RFC 9728 metadata, RFC 8414 / OIDC discovery), so
//! the common cases need no model reasoning and no web search.

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{AuthError, short};
use crate::flow::{OAuthConfig, TokenAuth};
use crate::http::{Guard, origin};
use crate::profiles::Profile;

/// Where a plan came from. Only `research` plans are validated by domain
/// (the others come from tern's own table or the server's own metadata).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Builtin,
    McpMetadata,
    WellKnown,
    Research,
    /// No OAuth: a key or token.
    None,
}

/// How tern gets a client ID for the sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// RFC 7591: tern registers itself, no setup.
    DynamicRegistration,
    /// A client ID metadata document (`[auth] client_metadata_url`).
    Cimd,
    /// One the user registered (`[auth.clients.<provider>]`).
    UserSupplied,
    /// One tern Cloud brokers (not on the local tier).
    TernCloud,
}

/// A structured answer, never prose (§6.7.1).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AuthPlan {
    /// `oauth2`, or `unknown` (go to research, then keys).
    pub method: String,
    pub service: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// A built-in profile's id or a name for the discovered server: what a
    /// connector names in `capabilities.oauth`.
    #[serde(default)]
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorize_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub userinfo_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_url: Option<String>,
    #[serde(default)]
    pub pkce: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientKind>,
    /// A client ID is available now (configured, registrable, CIMD).
    #[serde(default)]
    pub client_ready: bool,
    /// Tokens need a client secret.
    #[serde(default)]
    pub confidential: bool,
    #[serde(default)]
    pub scopes_supported: Vec<String>,
    /// The MCP resource the tokens are for (RFC 8707 / 9728).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(default)]
    pub api_hosts: Vec<String>,
    /// Confirmed by the issuer's own metadata (or a built-in profile).
    #[serde(default)]
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// Doc URLs a researched plan is based on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_auth: Option<TokenAuth>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_params: Vec<(String, String)>,
}

impl AuthPlan {
    fn unknown(service: &str, api_hosts: Vec<String>) -> Self {
        Self {
            method: "unknown".into(),
            service: service.to_owned(),
            source: Some(Source::None),
            api_hosts,
            ..Self::default()
        }
    }

    pub fn is_oauth(&self) -> bool {
        self.method == "oauth2"
    }

    /// Whether the user can sign in with no setup (§6.7.3's first row).
    pub fn no_setup(&self) -> bool {
        self.is_oauth() && self.client_ready && !self.confidential
    }

    /// The flow config for a client ID, once one is known.
    pub fn oauth_config(&self, client_id: &str, scopes: Vec<String>) -> Option<OAuthConfig> {
        Some(OAuthConfig {
            provider: self.provider.clone(),
            issuer: self.issuer.clone(),
            authorize_url: self.authorize_url.clone()?,
            token_url: self.token_url.clone()?,
            device_url: self.device_url.clone(),
            revocation_url: self.revocation_url.clone(),
            userinfo_url: self.userinfo_url.clone(),
            client_id: client_id.to_owned(),
            client_secret_secret: None,
            token_auth: self.token_auth.unwrap_or_default(),
            scopes,
            extra_params: self.extra_params.clone(),
            resource: self.resource.clone(),
            redirect_uri: None,
        })
    }
}

/// What the configuration offers discovery.
#[derive(Debug, Clone, Default)]
pub struct Clients {
    /// Providers with an `[auth.clients]` entry.
    pub configured: Vec<String>,
    /// `[auth] client_metadata_url` is set.
    pub cimd: bool,
}

pub fn from_profile(p: &Profile, clients: &Clients, api_hosts: Vec<String>) -> AuthPlan {
    let ready = clients.configured.iter().any(|c| c == p.id);
    AuthPlan {
        method: "oauth2".into(),
        service: p.name.to_owned(),
        source: Some(Source::Builtin),
        provider: p.id.to_owned(),
        issuer: p.issuer.map(Into::into),
        authorize_url: Some(p.authorize_url.into()),
        token_url: Some(p.token_url.into()),
        device_url: p.device_url.map(Into::into),
        revocation_url: p.revocation_url.map(Into::into),
        userinfo_url: p.userinfo_url.map(Into::into),
        registration_url: None,
        pkce: vec!["S256".into()],
        client: Some(ClientKind::UserSupplied),
        client_ready: ready,
        confidential: p.confidential,
        scopes_supported: p.base_scopes.iter().map(|s| (*s).to_owned()).collect(),
        resource: None,
        api_hosts: if api_hosts.is_empty() {
            p.api_hosts.iter().map(|h| (*h).to_owned()).collect()
        } else {
            api_hosts
        },
        verified: true,
        setup_url: Some(p.setup_url.into()),
        notes: p.notes.iter().map(|n| (*n).to_owned()).collect(),
        evidence: Vec::new(),
        token_auth: Some(p.token_auth),
        extra_params: p
            .extra_params
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

/// Run the checks in order. `url` is the API or MCP URL, when known;
/// `mcp` says it is an MCP server.
pub async fn discover(
    guard: &Guard,
    clients: &Clients,
    service: &str,
    url: Option<&str>,
    mcp: bool,
) -> Result<AuthPlan, AuthError> {
    let url = match url {
        Some(u) => {
            Some(Url::parse(u).map_err(|e| AuthError::Invalid(format!("{u:?} isn't a URL: {e}")))?)
        }
        None => None,
    };
    let host = url
        .as_ref()
        .and_then(|u| u.host_str())
        .map(str::to_ascii_lowercase);
    let api_hosts: Vec<String> = host.iter().cloned().collect();
    // 1. Built-in profiles. An MCP server signs in on its own terms, even
    // when it belongs to a service with a profile.
    if !mcp && let Some(p) = crate::profiles::find(service, host.as_deref()) {
        return Ok(from_profile(p, clients, api_hosts));
    }
    let Some(url) = url else {
        return Ok(AuthPlan::unknown(service, api_hosts));
    };
    // 2. The MCP authorization flow: protected resource metadata.
    if mcp && let Some(plan) = mcp_metadata(guard, clients, service, &url).await? {
        return Ok(plan);
    }
    // 3. RFC 8414 / OIDC on the API host, then its registrable domain.
    let mut issuers = vec![origin(&url)];
    if let Some(site) = host.as_deref().and_then(crate::domain::registrable)
        && Some(site.as_str()) != host.as_deref()
    {
        issuers.push(format!("https://{site}"));
    }
    for issuer in issuers {
        if let Some(meta) = as_metadata(guard, &issuer).await {
            let mut plan = plan_from_metadata(service, &meta, clients, Source::WellKnown)?;
            plan.api_hosts = api_hosts;
            return Ok(plan);
        }
    }
    // 4. Nothing: research, or the key flow.
    Ok(AuthPlan::unknown(service, api_hosts))
}

/// RFC 9728: the `resource_metadata` a 401 points at, or the well-known
/// document for the server's URL; then its authorization server.
async fn mcp_metadata(
    guard: &Guard,
    clients: &Clients,
    service: &str,
    url: &Url,
) -> Result<Option<AuthPlan>, AuthError> {
    let mut candidates: Vec<Url> = Vec::new();
    if let Some(u) = challenge_metadata(guard, url).await {
        candidates.push(u);
    }
    let path = url.path().trim_end_matches('/');
    for p in [
        format!("/.well-known/oauth-protected-resource{path}"),
        "/.well-known/oauth-protected-resource".to_owned(),
    ] {
        if let Ok(u) = url.join(&p)
            && !candidates.contains(&u)
        {
            candidates.push(u);
        }
    }
    for candidate in candidates {
        let Ok(prm) = guard.get_json(&candidate).await else {
            continue;
        };
        // §3.3: the document must be about this server.
        let resource = prm
            .get("resource")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !same_resource(resource, url) {
            tracing::warn!(resource = %short(resource, 100), url = %url, "protected resource metadata names another resource");
            continue;
        }
        let Some(issuer) = prm
            .get("authorization_servers")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(meta) = as_metadata(guard, issuer).await else {
            return Err(AuthError::Invalid(format!(
                "{} names the authorization server {issuer:?}, which publishes no usable metadata",
                origin(url)
            )));
        };
        let mut plan = plan_from_metadata(service, &meta, clients, Source::McpMetadata)?;
        plan.resource = Some(resource.to_owned());
        plan.api_hosts = url
            .host_str()
            .map(|h| vec![h.to_ascii_lowercase()])
            .unwrap_or_default();
        if let Some(scopes) = prm.get("scopes_supported").and_then(Value::as_array) {
            plan.scopes_supported = strings(scopes);
        }
        return Ok(Some(plan));
    }
    Ok(None)
}

fn same_resource(resource: &str, url: &Url) -> bool {
    let Ok(r) = Url::parse(resource) else {
        return false;
    };
    origin(&r) == origin(url) && r.path().trim_end_matches('/') == url.path().trim_end_matches('/')
}

/// Send an unauthenticated MCP `initialize`; a 401's `WWW-Authenticate`
/// may name the metadata document (on the server's own origin only).
async fn challenge_metadata(guard: &Guard, url: &Url) -> Option<Url> {
    let client = guard.client(url).await.ok()?;
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "tern", "version": env!("CARGO_PKG_VERSION")}}
    });
    let req = client
        .post(url.clone())
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .json(&body);
    let (status, headers, _) = guard.send("MCP probe", req).await.ok()?;
    if status != 401 {
        return None;
    }
    let challenge = headers
        .get_all(reqwest::header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| param(v, "resource_metadata"))?;
    let u = url.join(&challenge).ok()?;
    (origin(&u) == origin(url)).then_some(u)
}

/// `name="value"` from an auth challenge.
fn param(header: &str, name: &str) -> Option<String> {
    let at = header.find(&format!("{name}="))? + name.len() + 1;
    let rest = &header[at..];
    match rest.strip_prefix('"') {
        Some(q) => q.split('"').next().map(str::to_owned),
        None => rest.split([',', ' ']).next().map(str::to_owned),
    }
}

/// RFC 8414 §3 and OIDC discovery for `issuer`, accepting a document only
/// when its `issuer` is the one asked for (§3.3), so a server can't speak
/// for another.
pub async fn as_metadata(guard: &Guard, issuer: &str) -> Option<Value> {
    let base = Url::parse(issuer).ok()?;
    let path = base.path().trim_end_matches('/');
    let mut urls = vec![
        format!("/.well-known/oauth-authorization-server{path}"),
        format!("/.well-known/openid-configuration{path}"),
    ];
    if !path.is_empty() {
        urls.push(format!("{path}/.well-known/openid-configuration"));
    }
    for p in urls {
        let Ok(u) = base.join(&p) else { continue };
        let Ok(meta) = guard.get_json(&u).await else {
            continue;
        };
        let claimed = meta
            .get("issuer")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if claimed.trim_end_matches('/') == issuer.trim_end_matches('/') {
            return Some(meta);
        }
        tracing::warn!(issuer = %issuer, claimed = %short(claimed, 100), "metadata names another issuer; ignored");
    }
    None
}

fn strings(a: &[Value]) -> Vec<String> {
    a.iter()
        .filter_map(Value::as_str)
        .take(100)
        .map(|s| short(s, 200))
        .collect()
}

/// A provider name from an issuer: its registrable domain's first label
/// (`auth.notion.com` → `notion`).
pub fn provider_name(issuer: &str) -> String {
    let host = Url::parse(issuer)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    let site = crate::domain::registrable(&host).unwrap_or(host);
    let name: String = site
        .split('.')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(32)
        .collect::<String>()
        .to_ascii_lowercase();
    if name.is_empty() {
        "oauth".into()
    } else {
        name
    }
}

pub fn plan_from_metadata(
    service: &str,
    meta: &Value,
    clients: &Clients,
    source: Source,
) -> Result<AuthPlan, AuthError> {
    let s = |k: &str| meta.get(k).and_then(Value::as_str).map(str::to_owned);
    let list = |k: &str| {
        meta.get(k)
            .and_then(Value::as_array)
            .map(|a| strings(a))
            .unwrap_or_default()
    };
    let issuer = s("issuer").unwrap_or_default();
    let authorize = s("authorization_endpoint").ok_or_else(|| {
        AuthError::Invalid(format!("{issuer}: metadata has no authorization_endpoint"))
    })?;
    let token = s("token_endpoint")
        .ok_or_else(|| AuthError::Invalid(format!("{issuer}: metadata has no token_endpoint")))?;
    for u in [&authorize, &token] {
        // Plain http only from an issuer that is itself http, which the
        // guard allows only for `[auth] local_issuers`.
        if !u.starts_with("https://") && !issuer.starts_with("http://") {
            return Err(AuthError::Invalid(format!(
                "{issuer}: endpoint {u} isn't https"
            )));
        }
    }
    let pkce = list("code_challenge_methods_supported");
    let mut notes = Vec::new();
    if pkce.is_empty() {
        notes.push("The server doesn't say it supports PKCE; tern uses it anyway.".to_owned());
    } else if !pkce.iter().any(|m| m == "S256") {
        return Err(AuthError::Invalid(format!(
            "{issuer} supports only {} for PKCE, not S256; use a key instead",
            pkce.join(", ")
        )));
    }
    let registration = s("registration_endpoint");
    let cimd = meta
        .get("client_id_metadata_document_supported")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let provider = provider_name(&issuer);
    let configured = clients.configured.contains(&provider);
    let (client, ready) = if configured {
        (ClientKind::UserSupplied, true)
    } else if cimd && clients.cimd {
        (ClientKind::Cimd, true)
    } else if registration.is_some() {
        (ClientKind::DynamicRegistration, true)
    } else {
        (ClientKind::UserSupplied, false)
    };
    let methods = list("token_endpoint_auth_methods_supported");
    let public_ok = methods.is_empty() || methods.iter().any(|m| m == "none");
    let confidential = !configured && !public_ok;
    if confidential {
        notes.push("The server only accepts clients with a secret.".to_owned());
    }
    Ok(AuthPlan {
        method: "oauth2".into(),
        service: service.to_owned(),
        source: Some(source),
        provider,
        issuer: Some(issuer),
        authorize_url: Some(authorize),
        token_url: Some(token),
        device_url: s("device_authorization_endpoint"),
        revocation_url: s("revocation_endpoint"),
        userinfo_url: s("userinfo_endpoint"),
        registration_url: registration,
        pkce,
        client: Some(client),
        client_ready: ready,
        confidential,
        scopes_supported: list("scopes_supported"),
        resource: None,
        api_hosts: Vec::new(),
        verified: true,
        setup_url: None,
        notes,
        evidence: Vec::new(),
        token_auth: Some(TokenAuth::None),
        extra_params: Vec::new(),
    })
}

/// Check a plan the agent researched (§6.7.2): https, endpoints on the
/// API's registrable domain or a known identity provider, then confirmed
/// against the issuer's metadata when there is any. `Err` refuses it;
/// `Ok` says whether it was confirmed.
pub async fn validate_research(guard: &Guard, plan: &mut AuthPlan) -> Result<(), String> {
    use crate::domain::{Idp, Relation, check_endpoint};
    plan.verified = false;
    plan.source = Some(Source::Research);
    if plan.api_hosts.is_empty() {
        return Err("a researched plan must name the API hosts it is for (api_hosts)".into());
    }
    let authorize = plan
        .authorize_url
        .clone()
        .ok_or("a researched plan needs authorize_url")?;
    let token = plan
        .token_url
        .clone()
        .ok_or("a researched plan needs token_url")?;
    let mut tenant = false;
    let mut endpoints = vec![("authorize", authorize.clone()), ("token", token.clone())];
    for (what, u) in [
        ("device", &plan.device_url),
        ("revocation", &plan.revocation_url),
        ("userinfo", &plan.userinfo_url),
    ] {
        if let Some(u) = u {
            endpoints.push((what, u.clone()));
        }
    }
    for (what, u) in &endpoints {
        if check_endpoint(what, u, &plan.api_hosts)? == Relation::KnownIdp(Idp::Tenant) {
            tenant = true;
        }
    }
    // Confirm with the issuer's metadata: the plan's issuer, else the
    // authorize endpoint's origin.
    let issuer = plan.issuer.clone().unwrap_or_else(|| {
        Url::parse(&authorize)
            .map(|u| origin(&u))
            .unwrap_or_default()
    });
    if let Ok(issuer_url) = Url::parse(&issuer) {
        // A local issuer is one the user configured.
        if !guard.is_local(&issuer_url) {
            check_endpoint("issuer", issuer_url.as_str(), &plan.api_hosts)?;
        }
        if let Some(meta) = as_metadata(guard, &issuer).await {
            let same = |k: &str, want: &str| {
                meta.get(k)
                    .and_then(Value::as_str)
                    .map(|v| v.trim_end_matches('/'))
                    == Some(want.trim_end_matches('/'))
            };
            if !same("authorization_endpoint", &authorize) || !same("token_endpoint", &token) {
                return Err(format!(
                    "the issuer {issuer}'s own metadata lists different endpoints; use auth_discover's plan instead"
                ));
            }
            plan.issuer = Some(issuer);
            plan.verified = true;
        }
    }
    if tenant && !plan.verified {
        return Err(
            "the endpoints are on an identity provider's tenant domain, which anyone can register; without the issuer's metadata to confirm them they can't be used"
                .into(),
        );
    }
    if !plan.verified {
        plan.notes.push(
            "Unverified: the service publishes no metadata to confirm these sign-in addresses."
                .into(),
        );
    }
    if plan.provider.is_empty() {
        plan.provider = provider_name(&token);
    }
    plan.method = "oauth2".into();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_params() {
        let h = r#"Bearer error="invalid_token", resource_metadata="https://mcp.x.com/.well-known/oauth-protected-resource""#;
        assert_eq!(
            param(h, "resource_metadata").as_deref(),
            Some("https://mcp.x.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(param("Bearer realm=x", "realm").as_deref(), Some("x"));
        assert_eq!(provider_name("https://auth.notion.com"), "notion");
        assert_eq!(
            provider_name("https://login.example.co.uk/tenant"),
            "example"
        );
    }
}
