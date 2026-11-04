//! MCP server status and the checks every write path shares
//! (docs/mcp-management-spec.md). The client itself lives in `tern-mcp`.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::config::{McpServerConfig, McpTransport};

/// Longest server name, so `mcp__<server>__<tool>` stays under the
/// 64-character tool name limit for most tool names.
pub const MAX_NAME: usize = 24;

/// Where a server is, as clients see it. Times are Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpState {
    Disabled,
    NotStarted,
    Connecting,
    Running {
        tools: usize,
        since: i64,
    },
    Unavailable {
        error: String,
        retry_at: Option<i64>,
        failures: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolInfo {
    /// The name the model sees, `mcp__<server>__<tool>`.
    pub exposed: String,
    /// The server's own name for it.
    pub remote: String,
    pub description: String,
    /// Listed in `disabled_tools`, so hidden from the model.
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpStatus {
    pub name: String,
    /// Secret names only, never values.
    pub config: McpServerConfig,
    pub state: McpState,
    pub tools: Vec<McpToolInfo>,
    pub missing_secrets: Vec<String>,
    /// Name and version from the server's `initialize` reply.
    pub server_info: Option<String>,
    /// The last lines a `stdio` server wrote to stderr, redacted.
    pub stderr_tail: Vec<String>,
}

/// A successful test connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpVerified {
    pub tools: Vec<McpToolInfo>,
    pub server_info: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpErrorKind {
    CommandNotFound,
    SpawnFailed,
    ExitedEarly,
    Timeout,
    /// HTTP 401 or 403.
    Unauthorized,
    Unreachable,
    Protocol,
    MissingSecret,
    /// The config itself is wrong (no `command`, a bad URL, …).
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpVerifyError {
    pub kind: McpErrorKind,
    pub message: String,
    pub stderr_tail: Vec<String>,
}

impl std::fmt::Display for McpVerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for McpVerifyError {}

/// A server read from a pasted config, for the user to review. Nothing is
/// saved until the add flow does it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedServer {
    pub name: String,
    pub config: McpServerConfig,
    /// Secret name → the value found in the paste, or `None` when it was
    /// missing or a placeholder and the user must enter it.
    pub secrets: BTreeMap<String, Option<String>>,
    pub warnings: Vec<String>,
}

/// The quick toggles of `PATCH /v1/mcp/{name}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpPatch {
    pub enabled: Option<bool>,
    pub disabled_tools: Option<Vec<String>>,
    pub requires_approval: Option<bool>,
    pub idempotent: Option<bool>,
}

/// 1–24 characters from `[a-z0-9_-]`. Checked on write, not on load, so
/// existing configs keep working.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(format!(
            "MCP server names are 1–{MAX_NAME} characters, got {}",
            name.len()
        ));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        return Err(format!(
            "MCP server name {name:?} may use only a-z, 0-9, _ and -"
        ));
    }
    Ok(())
}

/// Everything a write checks besides the name: the transport has what it
/// needs, secret names are valid, nothing secret sits in plain config, and
/// HTTP credentials never go to a public host in cleartext.
pub fn check_config(cfg: &McpServerConfig) -> Result<(), String> {
    match cfg.transport {
        McpTransport::Stdio => {
            if cfg.command.as_deref().is_none_or(|c| c.trim().is_empty()) {
                return Err("a stdio server needs `command`".into());
            }
            if cfg.url.is_some()
                || cfg.bearer_token_secret.is_some()
                || !cfg.header_secrets.is_empty()
            {
                return Err(
                    "`url`, `bearer_token_secret` and `header_secrets` are for http servers".into(),
                );
            }
        }
        McpTransport::Http => {
            let url = cfg.url.as_deref().ok_or("an http server needs `url`")?;
            check_url(
                url,
                cfg.bearer_token_secret.is_some() || !cfg.header_secrets.is_empty(),
            )?;
            if cfg.command.is_some()
                || !cfg.args.is_empty()
                || cfg.cwd.is_some()
                || !cfg.env.is_empty()
            {
                return Err("`command`, `args`, `cwd` and `env` are for stdio servers".into());
            }
        }
    }
    for name in cfg.secret_names() {
        crate::secrets::check_name(name).map_err(|e| e.to_string())?;
    }
    for header in cfg.header_secrets.keys() {
        if header.is_empty()
            || !header
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(format!("{header:?} is not a valid header name"));
        }
    }
    for (k, v) in &cfg.env {
        if looks_like_secret(k, v) {
            return Err(format!(
                "`{k}` looks like a secret. Store it as a secret instead (add it to `env_secrets`)."
            ));
        }
    }
    for a in &cfg.args {
        if looks_like_secret("", a) {
            return Err(format!(
                "an argument looks like a secret ({}…). Pass it through `env_secrets` instead.",
                a.chars().take(6).collect::<String>()
            ));
        }
    }
    Ok(())
}

/// `http` only to loopback and private hosts when credentials go along.
fn check_url(url: &str, sends_credentials: bool) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("bad url {url:?}: {e}"))?;
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if !sends_credentials || is_local_host(&parsed) => Ok(()),
        "http" => Err(format!(
            "{url} is plain http to a public host; use https so the token isn't sent in the clear"
        )),
        other => Err(format!(
            "unsupported url scheme {other:?}; use http or https"
        )),
    }
}

fn is_local_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v)) => v.is_loopback() || v.is_private() || v.is_link_local(),
        Ok(IpAddr::V6(v)) => v.is_loopback() || (v.segments()[0] & 0xfe00) == 0xfc00,
        Err(_) => false,
    }
}

/// Prefixes of well-known credentials.
const KEY_PREFIXES: &[&str] = &[
    "sk-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xapp-",
    "AKIA",
    "AIza",
    "lin_api_",
    "ntn_",
    "secret_",
    "pk_live_",
    "sk_live_",
    "rk_live_",
];

/// Whether `value` (under env var or header `name`) looks like a credential:
/// a known key prefix, or a long random-looking value under a name that
/// says `TOKEN`, `KEY`, `SECRET` or `PASSWORD`.
pub fn looks_like_secret(name: &str, value: &str) -> bool {
    let v = value.trim();
    let v = v.strip_prefix("Bearer ").unwrap_or(v);
    if KEY_PREFIXES
        .iter()
        .any(|p| v.starts_with(p) && v.len() >= p.len() + 12)
    {
        return true;
    }
    let upper = name.to_ascii_uppercase();
    let secretish = ["TOKEN", "KEY", "SECRET", "PASSWORD", "AUTH"]
        .iter()
        .any(|w| upper.contains(w));
    secretish && v.len() > 24 && !v.contains(char::is_whitespace) && entropy(v) >= 3.5
}

/// Shannon entropy in bits per character.
fn entropy(s: &str) -> f64 {
    let mut counts = BTreeMap::<char, usize>::new();
    for c in s.chars() {
        *counts.entry(c).or_default() += 1;
    }
    let n = s.chars().count() as f64;
    counts
        .values()
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

/// Values people paste in place of a real key: `<YOUR_TOKEN>`,
/// `${GITHUB_TOKEN}`, `your-api-key-here`, empty.
pub fn is_placeholder(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return true;
    }
    if (v.starts_with('<') && v.ends_with('>'))
        || (v.starts_with("${") && v.ends_with('}'))
        || (v.starts_with('{') && v.ends_with('}'))
        || v.starts_with('$')
    {
        return true;
    }
    let lower = v.to_ascii_lowercase();
    [
        "your",
        "replace",
        "insert",
        "xxxx",
        "placeholder",
        "changeme",
        "todo",
        "<",
        "...",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio() -> McpServerConfig {
        McpServerConfig {
            command: Some("npx".into()),
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn names() {
        assert!(check_name("github").is_ok());
        assert!(check_name("my-server_2").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name("GitHub").is_err());
        assert!(check_name("a.b").is_err());
        assert!(check_name(&"x".repeat(25)).is_err());
    }

    #[test]
    fn secrets_in_plain_config_are_refused() {
        let mut c = stdio();
        c.env.insert("LOG_LEVEL".into(), "warn".into());
        assert!(check_config(&c).is_ok());
        c.env
            .insert("GITHUB_TOKEN".into(), "ghp_abcdefghijklmnopqrstuvwx".into());
        let e = check_config(&c).unwrap_err();
        assert!(e.contains("GITHUB_TOKEN"), "{e}");

        let mut c = stdio();
        c.args = vec!["--key".into(), "sk-ant-api03-abcdefghijklmnop".into()];
        assert!(check_config(&c).is_err());

        assert!(looks_like_secret(
            "API_KEY",
            "q8Z2fL0pXr7TmW4vYb9NcK3dHs6Ju1Ea"
        ));
        assert!(!looks_like_secret("API_KEY", "short"));
        assert!(!looks_like_secret(
            "PATH_PREFIX",
            "q8Z2fL0pXr7TmW4vYb9NcK3dHs6Ju1Ea"
        ));
        assert!(!looks_like_secret("MODE", "sk-"));
    }

    #[test]
    fn transports_need_their_fields() {
        assert!(check_config(&McpServerConfig::default()).is_err());
        let http = |url: &str, bearer: bool| McpServerConfig {
            transport: McpTransport::Http,
            enabled: true,
            url: Some(url.into()),
            bearer_token_secret: bearer.then(|| "TOKEN".into()),
            ..Default::default()
        };
        assert!(check_config(&http("https://mcp.example.com/mcp", true)).is_ok());
        assert!(check_config(&http("http://mcp.example.com/mcp", false)).is_ok());
        assert!(check_config(&http("http://mcp.example.com/mcp", true)).is_err());
        assert!(check_config(&http("http://127.0.0.1:8080/mcp", true)).is_ok());
        assert!(check_config(&http("http://192.168.1.4/mcp", true)).is_ok());
        assert!(check_config(&http("ftp://x", false)).is_err());
        let mut c = http("https://x.example", false);
        c.command = Some("x".into());
        assert!(check_config(&c).is_err());
    }

    #[test]
    fn placeholders() {
        for p in [
            "",
            "<YOUR_TOKEN>",
            "${GITHUB_TOKEN}",
            "your-api-key-here",
            "xxxxxxxx",
            "$TOKEN",
        ] {
            assert!(is_placeholder(p), "{p}");
        }
        assert!(!is_placeholder("ghp_abcdefghijklmnopqrstuvwx"));
    }
}
