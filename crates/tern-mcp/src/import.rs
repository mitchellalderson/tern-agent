//! Reading the `mcpServers` JSON that MCP servers document for Claude
//! Desktop, Cursor and others (docs/mcp-management-spec.md §4.6). Nothing is
//! saved here: the drafts go to the add flow for review.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use tern_core::config::{McpServerConfig, McpTransport};
use tern_core::mcp::{ImportedServer, MAX_NAME, is_placeholder, looks_like_secret};

/// Servers in `text`: `{"mcpServers": {...}}` (or `"servers"`), the inner
/// map on its own, or a single server object.
pub fn import(text: &str) -> Result<Vec<ImportedServer>, String> {
    let root: Value =
        serde_json::from_str(text.trim()).map_err(|e| format!("not valid JSON: {e}"))?;
    let Value::Object(root) = root else {
        return Err("expected a JSON object".into());
    };
    let entries: Vec<(String, Value)> = match root.get("mcpServers").or_else(|| root.get("servers"))
    {
        Some(Value::Object(m)) => m.clone().into_iter().collect(),
        Some(_) => return Err("`mcpServers` should be an object".into()),
        None if is_server(&root) => vec![("server".into(), Value::Object(root))],
        None => root.into_iter().collect(),
    };
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for (name, v) in entries {
        let Value::Object(obj) = v else {
            skipped.push(format!("{name}: not a server object, skipped"));
            continue;
        };
        match server(&name, &obj) {
            Ok(s) => out.push(s),
            Err(e) => skipped.push(format!("{name}: {e}")),
        }
    }
    match out.first_mut() {
        Some(first) => {
            first.warnings.extend(skipped);
            Ok(out)
        }
        None if skipped.is_empty() => Err("no MCP servers found".into()),
        None => Err(skipped.join("; ")),
    }
}

fn is_server(obj: &Map<String, Value>) -> bool {
    obj.contains_key("command") || obj.contains_key("url")
}

fn server(raw_name: &str, obj: &Map<String, Value>) -> Result<ImportedServer, String> {
    let mut warnings = Vec::new();
    let name = clean_name(raw_name);
    if name != raw_name {
        warnings.push(format!("renamed {raw_name:?} to {name:?}"));
    }
    let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
    if kind == "sse" {
        return Err(
            "tern doesn't support the SSE transport yet; check whether the server \
                    also offers streamable HTTP"
                .into(),
        );
    }
    let mut cfg = McpServerConfig {
        enabled: !obj
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ..Default::default()
    };
    let mut secrets = BTreeMap::new();
    if let Some(url) = obj.get("url").and_then(Value::as_str) {
        cfg.transport = McpTransport::Http;
        cfg.url = Some(url.to_owned());
        let prefix = name.to_ascii_uppercase().replace('-', "_");
        for (header, value) in strings(obj.get("headers")) {
            if header.eq_ignore_ascii_case("authorization") {
                let token = value.strip_prefix("Bearer ").unwrap_or(&value).trim();
                let secret = format!("{prefix}_TOKEN");
                cfg.bearer_token_secret = Some(secret.clone());
                secrets.insert(secret, real(token));
            } else {
                let secret = format!("{prefix}_{}", header.to_ascii_uppercase().replace('-', "_"));
                cfg.header_secrets.insert(header, secret.clone());
                secrets.insert(secret, real(&value));
            }
        }
    } else if let Some(command) = obj.get("command").and_then(Value::as_str) {
        if !matches!(kind, "" | "stdio") {
            return Err(format!("unknown transport type {kind:?}"));
        }
        cfg.command = Some(command.to_owned());
        cfg.args = obj
            .get("args")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if cfg.args.iter().any(|a| looks_like_secret("", a)) {
            warnings.push(
                "an argument looks like a secret; pass it as an environment variable instead"
                    .into(),
            );
        }
        cfg.cwd = obj.get("cwd").and_then(Value::as_str).map(str::to_owned);
        for (key, value) in strings(obj.get("env")) {
            let secretish = looks_like_secret(&key, &value)
                || (is_placeholder(&value) && (!value.trim().is_empty() || names_secret(&key)));
            if secretish {
                cfg.env_secrets.push(key.clone());
                secrets.insert(key, real(&value));
            } else {
                cfg.env.insert(key, value);
            }
        }
    } else {
        return Err("needs `command` or `url`".into());
    }
    for (secret, value) in &secrets {
        if value.is_none() {
            warnings.push(format!("enter a value for {secret}"));
        }
    }
    Ok(ImportedServer {
        name,
        config: cfg,
        secrets,
        warnings,
    })
}

/// A value worth keeping, or `None` for a placeholder.
fn real(value: &str) -> Option<String> {
    (!is_placeholder(value)).then(|| value.trim().to_owned())
}

fn names_secret(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    ["TOKEN", "KEY", "SECRET", "PASSWORD", "AUTH"]
        .iter()
        .any(|w| k.contains(w))
}

fn strings(v: Option<&Value>) -> Vec<(String, String)> {
    v.and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    let v = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
                    (k.clone(), v)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Lowercase `[a-z0-9_-]`, at most [`MAX_NAME`] characters.
fn clean_name(raw: &str) -> String {
    let mut name: String = raw
        .trim()
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '_' | '-') => c,
            _ => '-',
        })
        .collect();
    name.truncate(MAX_NAME);
    let name = name.trim_matches('-').to_owned();
    if name.is_empty() {
        "server".into()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_desktop_json() {
        let got = import(
            r#"{
              "mcpServers": {
                "GitHub": {
                  "command": "docker",
                  "args": ["run", "-i", "--rm", "ghcr.io/github/github-mcp-server"],
                  "env": {
                    "GITHUB_PERSONAL_ACCESS_TOKEN": "<YOUR_TOKEN>",
                    "LOG_LEVEL": "warn"
                  }
                },
                "fs": {
                  "command": "npx",
                  "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
                  "env": { "API_KEY": "sk-ant-api03-abcdefghijklmnopqrstuv" }
                },
                "old": { "type": "sse", "url": "https://example.com/sse" }
              }
            }"#,
        )
        .unwrap();
        assert_eq!(got.len(), 2);
        let by = |n: &str| got.iter().find(|s| s.name == n).unwrap();
        assert!(
            got.iter()
                .flat_map(|s| &s.warnings)
                .any(|w| w.contains("SSE"))
        );
        let fs = by("fs");
        assert_eq!(fs.config.env_secrets, ["API_KEY"]);
        assert_eq!(
            fs.secrets["API_KEY"].as_deref(),
            Some("sk-ant-api03-abcdefghijklmnopqrstuv")
        );
        assert!(fs.config.env.is_empty());

        let gh = by("github");
        assert_eq!(gh.config.command.as_deref(), Some("docker"));
        assert_eq!(gh.config.env_secrets, ["GITHUB_PERSONAL_ACCESS_TOKEN"]);
        assert_eq!(gh.secrets["GITHUB_PERSONAL_ACCESS_TOKEN"], None);
        assert_eq!(gh.config.env["LOG_LEVEL"], "warn");
        assert!(tern_core::mcp::check_config(&gh.config).is_ok());
    }

    #[test]
    fn http_with_bearer_and_headers() {
        let got = import(
            r#"{ "linear": { "type": "http", "url": "https://mcp.linear.app/mcp",
                 "headers": { "Authorization": "Bearer lin_api_abcdefghijklmnop", "X-Team": "${TEAM}" } } }"#,
        )
        .unwrap();
        let s = &got[0];
        assert_eq!(s.config.transport, McpTransport::Http);
        assert_eq!(
            s.config.bearer_token_secret.as_deref(),
            Some("LINEAR_TOKEN")
        );
        assert_eq!(
            s.secrets["LINEAR_TOKEN"].as_deref(),
            Some("lin_api_abcdefghijklmnop")
        );
        assert_eq!(s.config.header_secrets["X-Team"], "LINEAR_X_TEAM");
        assert_eq!(s.secrets["LINEAR_X_TEAM"], None);
        assert!(tern_core::mcp::check_config(&s.config).is_ok());
    }

    #[test]
    fn single_server_and_errors() {
        let got = import(r#"{"command": "uvx", "args": ["mcp-server-time"]}"#).unwrap();
        assert_eq!(got[0].name, "server");
        assert_eq!(got[0].config.args, ["mcp-server-time"]);
        assert!(import("nope").is_err());
        assert!(import("[]").is_err());
        let e =
            import(r#"{"mcpServers": {"x": {"type": "sse", "url": "https://x"}}}"#).unwrap_err();
        assert!(e.contains("SSE"), "{e}");
    }
}
