//! Built-in OAuth profiles (tooling spec §6.2, §6.7.1 step 1): the
//! services people connect most, so discovery needs no network and no
//! model reasoning for them. Endpoints are the providers' documented ones.

use crate::flow::TokenAuth;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub id: &'static str,
    pub name: &'static str,
    /// Other names the service goes by (`gmail` → google).
    pub aliases: &'static [&'static str],
    /// API hosts the tokens are for (`*.` covers subdomains).
    pub api_hosts: &'static [&'static str],
    pub issuer: Option<&'static str>,
    pub authorize_url: &'static str,
    pub token_url: &'static str,
    pub device_url: Option<&'static str>,
    pub revocation_url: Option<&'static str>,
    pub userinfo_url: Option<&'static str>,
    /// How the token endpoint wants the client to authenticate.
    pub token_auth: TokenAuth,
    /// The provider won't issue tokens to a public client (it needs a
    /// client secret a desktop app can't keep).
    pub confidential: bool,
    /// Scopes to add to whatever the connector asks for.
    pub base_scopes: &'static [&'static str],
    pub extra_params: &'static [(&'static str, &'static str)],
    /// Where the user registers their own OAuth app.
    pub setup_url: &'static str,
    /// Plain-words caveats for the choice card.
    pub notes: &'static [&'static str],
}

pub const PROFILES: &[Profile] = &[
    Profile {
        id: "google",
        name: "Google",
        aliases: &[
            "gmail",
            "google calendar",
            "gcal",
            "google drive",
            "gdrive",
            "google docs",
            "google sheets",
            "youtube",
        ],
        api_hosts: &["*.googleapis.com"],
        issuer: Some("https://accounts.google.com"),
        authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
        token_url: "https://oauth2.googleapis.com/token",
        device_url: Some("https://oauth2.googleapis.com/device/code"),
        revocation_url: Some("https://oauth2.googleapis.com/revoke"),
        userinfo_url: Some("https://openidconnect.googleapis.com/v1/userinfo"),
        token_auth: TokenAuth::Post,
        confidential: false,
        base_scopes: &["openid", "email"],
        extra_params: &[("access_type", "offline"), ("prompt", "consent")],
        setup_url: "https://console.cloud.google.com/apis/credentials",
        notes: &[
            "Needs your own OAuth client (type \"Desktop app\") in Google Cloud.",
            "While the Google project is in Testing, sign-ins expire after 7 days; publishing it (no review needed for personal use) avoids that.",
        ],
    },
    Profile {
        id: "microsoft",
        name: "Microsoft",
        aliases: &[
            "outlook",
            "office 365",
            "microsoft 365",
            "onedrive",
            "teams",
            "microsoft graph",
        ],
        api_hosts: &["graph.microsoft.com"],
        issuer: None,
        authorize_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
        device_url: Some("https://login.microsoftonline.com/common/oauth2/v2.0/devicecode"),
        revocation_url: None,
        userinfo_url: Some("https://graph.microsoft.com/oidc/userinfo"),
        token_auth: TokenAuth::None,
        confidential: false,
        base_scopes: &["openid", "email", "offline_access"],
        extra_params: &[],
        setup_url: "https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade",
        notes: &[
            "Needs your own app registration (a public client with a http://127.0.0.1 redirect).",
        ],
    },
    Profile {
        id: "github",
        name: "GitHub",
        aliases: &[],
        api_hosts: &["api.github.com"],
        issuer: None,
        authorize_url: "https://github.com/login/oauth/authorize",
        token_url: "https://github.com/login/oauth/access_token",
        device_url: Some("https://github.com/login/device/code"),
        revocation_url: None,
        userinfo_url: Some("https://api.github.com/user"),
        token_auth: TokenAuth::Post,
        confidential: false,
        base_scopes: &[],
        extra_params: &[],
        setup_url: "https://github.com/settings/developers",
        notes: &[
            "Needs your own OAuth app with device flow enabled; a fine-grained personal access token is the quicker alternative.",
        ],
    },
    Profile {
        id: "slack",
        name: "Slack",
        aliases: &[],
        api_hosts: &["slack.com"],
        issuer: Some("https://slack.com"),
        authorize_url: "https://slack.com/oauth/v2/authorize",
        token_url: "https://slack.com/api/oauth.v2.access",
        device_url: None,
        revocation_url: Some("https://slack.com/api/auth.revoke"),
        userinfo_url: Some("https://slack.com/api/openid.connect.userInfo"),
        token_auth: TokenAuth::Post,
        confidential: true,
        base_scopes: &[],
        extra_params: &[],
        setup_url: "https://api.slack.com/apps",
        notes: &[
            "Slack apps need a client secret; a bot or user token from your own Slack app is simpler.",
        ],
    },
    Profile {
        id: "notion",
        name: "Notion",
        aliases: &[],
        api_hosts: &["api.notion.com"],
        issuer: None,
        authorize_url: "https://api.notion.com/v1/oauth/authorize",
        token_url: "https://api.notion.com/v1/oauth/token",
        device_url: None,
        revocation_url: None,
        userinfo_url: None,
        token_auth: TokenAuth::Basic,
        confidential: true,
        base_scopes: &[],
        extra_params: &[("owner", "user")],
        setup_url: "https://www.notion.so/profile/integrations",
        notes: &[
            "Notion's API OAuth needs a client secret; an internal integration token is simpler. Notion's MCP server (mcp.notion.com) signs in with no setup.",
        ],
    },
    Profile {
        id: "linear",
        name: "Linear",
        aliases: &[],
        api_hosts: &["api.linear.app"],
        issuer: None,
        authorize_url: "https://linear.app/oauth/authorize",
        token_url: "https://api.linear.app/oauth/token",
        device_url: None,
        revocation_url: Some("https://api.linear.app/oauth/revoke"),
        userinfo_url: None,
        token_auth: TokenAuth::None,
        confidential: false,
        base_scopes: &[],
        extra_params: &[],
        setup_url: "https://linear.app/settings/api/applications/new",
        notes: &[
            "Needs your own OAuth application; a personal API key is the quicker alternative. Linear's MCP server signs in with no setup.",
        ],
    },
    Profile {
        id: "atlassian",
        name: "Atlassian",
        aliases: &["jira", "confluence"],
        api_hosts: &["api.atlassian.com", "*.atlassian.net"],
        issuer: None,
        authorize_url: "https://auth.atlassian.com/authorize",
        token_url: "https://auth.atlassian.com/oauth/token",
        device_url: None,
        revocation_url: None,
        userinfo_url: Some("https://api.atlassian.com/me"),
        token_auth: TokenAuth::Post,
        confidential: true,
        base_scopes: &["offline_access"],
        extra_params: &[("audience", "api.atlassian.com"), ("prompt", "consent")],
        setup_url: "https://developer.atlassian.com/console/myapps/",
        notes: &[
            "Atlassian apps need a client secret; an API token (with your email, as basic auth) is simpler.",
        ],
    },
    Profile {
        id: "dropbox",
        name: "Dropbox",
        aliases: &[],
        api_hosts: &["api.dropboxapi.com", "content.dropboxapi.com"],
        issuer: Some("https://www.dropbox.com"),
        authorize_url: "https://www.dropbox.com/oauth2/authorize",
        token_url: "https://api.dropboxapi.com/oauth2/token",
        device_url: None,
        revocation_url: None,
        userinfo_url: None,
        token_auth: TokenAuth::None,
        confidential: false,
        base_scopes: &[],
        extra_params: &[("token_access_type", "offline")],
        setup_url: "https://www.dropbox.com/developers/apps",
        notes: &["Needs your own Dropbox app (PKCE, no secret)."],
    },
];

pub fn by_id(id: &str) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.id == id)
}

/// The profile for a service name or alias (`Gmail`, `google`), or for an
/// API host one of them covers.
pub fn find(service: &str, host: Option<&str>) -> Option<&'static Profile> {
    let s = service.trim().to_ascii_lowercase();
    let by_name = PROFILES
        .iter()
        .find(|p| p.id == s || p.name.eq_ignore_ascii_case(&s) || p.aliases.contains(&s.as_str()));
    by_name.or_else(|| {
        let host = host?.to_ascii_lowercase();
        PROFILES
            .iter()
            .find(|p| p.api_hosts.iter().any(|h| host_matches(h, &host)))
    })
}

/// `*.example.com` covers subdomains; anything else is exact.
pub fn host_matches(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(base) => host.len() > base.len() + 1 && host.ends_with(&format!(".{base}")),
        None => pattern.eq_ignore_ascii_case(host),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_by_name_alias_and_host() {
        assert_eq!(find("Gmail", None).map(|p| p.id), Some("google"));
        assert_eq!(find("jira", None).map(|p| p.id), Some("atlassian"));
        assert_eq!(
            find("my thing", Some("gmail.googleapis.com")).map(|p| p.id),
            Some("google")
        );
        assert_eq!(find("x", Some("googleapis.com.evil.test")), None);
        assert_eq!(find("x", Some("evilgoogleapis.com")), None);
        for p in PROFILES {
            for u in [p.authorize_url, p.token_url] {
                assert!(u.starts_with("https://"), "{u}");
            }
        }
    }
}
