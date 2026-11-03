//! Validating the table a connector file returns (spec §10.2).

use mlua::{Function, Lua, LuaSerdeExt, Table, Value};

use crate::host::LoadError;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// Hosts `ctx.http` may reach (`api.example.com` or `*.example.com`).
    pub http: Vec<String>,
    /// Secret names `ctx.secret` may read.
    pub secrets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub idempotent: bool,
    pub requires_approval: bool,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConnectorSpec {
    pub name: String,
    pub description: String,
    pub capabilities: Capabilities,
    pub tools: Vec<ToolDef>,
}

impl ConnectorSpec {
    /// Human-readable capability summary for approval prompts.
    pub fn capability_summary(&self) -> String {
        let list = |v: &[String]| {
            if v.is_empty() {
                "none".to_owned()
            } else {
                v.join(", ")
            }
        };
        let tools: Vec<&str> = self.tools.iter().map(|t| t.name.as_str()).collect();
        format!(
            "http hosts: {}; secrets: {}; tools: {}",
            list(&self.capabilities.http),
            list(&self.capabilities.secrets),
            tools.join(", ")
        )
    }
}

/// Lowercase identifier usable inside a tool name: `[a-z][a-z0-9_]*`, ≤ 32.
pub fn valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    s.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn invalid(msg: impl Into<String>) -> LoadError {
    LoadError::Invalid(msg.into())
}

fn string_list(t: Option<Table>, what: &str) -> Result<Vec<String>, LoadError> {
    let Some(t) = t else {
        return Ok(Vec::new());
    };
    t.sequence_values::<String>()
        .collect::<mlua::Result<Vec<_>>>()
        .map_err(|_| invalid(format!("{what} must be a list of strings")))
}

/// `api.example.com` or `*.example.com`; nothing broader.
fn check_hosts(hosts: &[String]) -> Result<(), LoadError> {
    for h in hosts {
        let bare = h.strip_prefix("*.").unwrap_or(h);
        let valid = !bare.is_empty()
            && bare.contains('.')
            && bare
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
            || bare.parse::<std::net::IpAddr>().is_ok()
            || bare == "localhost";
        if !valid {
            return Err(invalid(format!(
                "capabilities.http entry {h:?} must be a host name like \"api.example.com\" or \"*.example.com\" (no \"*\" on its own, schemes, ports, or paths)"
            )));
        }
    }
    Ok(())
}

pub fn parse(lua: &Lua, t: &Table) -> Result<(ConnectorSpec, Vec<(String, Function)>), LoadError> {
    let get_str = |key: &str| -> Result<String, LoadError> {
        match t.raw_get::<Value>(key) {
            Ok(Value::String(s)) => Ok(s.to_string_lossy()),
            _ => Err(invalid(format!("`{key}` must be a string"))),
        }
    };
    let name = get_str("name")?;
    if !valid_ident(&name) {
        return Err(invalid(format!(
            "name {name:?} must match [a-z][a-z0-9_]* (max 32 chars)"
        )));
    }
    let description = get_str("description")?;

    let caps: Option<Table> = t
        .raw_get("capabilities")
        .map_err(|_| invalid("`capabilities` must be a table"))?;
    let capabilities = match caps {
        None => Capabilities::default(),
        Some(c) => Capabilities {
            http: string_list(
                c.raw_get("http")
                    .map_err(|_| invalid("capabilities.http must be a list"))?,
                "capabilities.http",
            )?,
            secrets: string_list(
                c.raw_get("secrets")
                    .map_err(|_| invalid("capabilities.secrets must be a list"))?,
                "capabilities.secrets",
            )?,
        },
    };

    check_hosts(&capabilities.http)?;

    let tools: Table = t
        .raw_get::<Option<Table>>("tools")
        .ok()
        .flatten()
        .ok_or_else(|| invalid("`tools` must be a table of tool definitions"))?;
    let mut defs = Vec::new();
    let mut runs = Vec::new();
    for pair in tools.pairs::<String, Table>() {
        let (tool, def) =
            pair.map_err(|_| invalid("each entry in `tools` must be `name = { ... }`"))?;
        if !valid_ident(&tool) {
            return Err(invalid(format!(
                "tool name {tool:?} must match [a-z][a-z0-9_]* (max 32 chars)"
            )));
        }
        let ctx = |m: &str| invalid(format!("tools.{tool}: {m}"));
        let description: String = def
            .raw_get::<Option<String>>("description")
            .ok()
            .flatten()
            .ok_or_else(|| ctx("`description` must be a string"))?;
        let run: Function = def
            .raw_get::<Option<Function>>("run")
            .ok()
            .flatten()
            .ok_or_else(|| ctx("`run` must be a function"))?;
        let flag = |k: &str| -> Result<bool, LoadError> {
            match def.raw_get::<Value>(k) {
                Ok(Value::Nil) => Ok(false),
                Ok(Value::Boolean(b)) => Ok(b),
                _ => Err(ctx(&format!("`{k}` must be a boolean"))),
            }
        };
        let params_value: Value = def
            .raw_get("params")
            .map_err(|_| ctx("`params` is unreadable"))?;
        let params: serde_json::Value = match params_value {
            Value::Nil => serde_json::json!({ "type": "object", "properties": {} }),
            v @ Value::Table(_) => lua
                .from_value_with(
                    v,
                    mlua::serde::DeserializeOptions::new().encode_empty_tables_as_array(false),
                )
                .map_err(|e| ctx(&format!("`params` is not valid JSON: {e}")))?,
            _ => return Err(ctx("`params` must be a JSON Schema table")),
        };
        check_schema(&params).map_err(|m| ctx(&format!("params: {m}")))?;
        defs.push(ToolDef {
            name: tool.clone(),
            description,
            idempotent: flag("idempotent")?,
            requires_approval: flag("requires_approval")?,
            params,
        });
        runs.push((tool, run));
    }
    if defs.is_empty() {
        return Err(invalid("`tools` is empty"));
    }
    defs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((
        ConnectorSpec {
            name,
            description,
            capabilities,
            tools: defs,
        },
        runs,
    ))
}

/// The subset of JSON Schema checks that catch real mistakes: an object
/// schema whose properties are schemas and whose `required` names exist.
pub fn check_schema(s: &serde_json::Value) -> Result<(), String> {
    let obj = s.as_object().ok_or("must be an object")?;
    if obj.get("type").and_then(|t| t.as_str()) != Some("object") {
        return Err("top-level `type` must be \"object\"".into());
    }
    let props = match obj.get("properties") {
        None => None,
        Some(serde_json::Value::Object(p)) => Some(p),
        Some(_) => return Err("`properties` must be a table of schemas".into()),
    };
    if let Some(p) = props {
        for (k, v) in p {
            let Some(v) = v.as_object() else {
                return Err(format!("properties.{k} must be a schema table"));
            };
            if let Some(t) = v.get("type")
                && !t.is_string()
                && !t.is_array()
            {
                return Err(format!("properties.{k}.type must be a string"));
            }
        }
    }
    if let Some(req) = obj.get("required") {
        let req = req.as_array().ok_or("`required` must be a list")?;
        for r in req {
            let name = r.as_str().ok_or("`required` entries must be strings")?;
            if !props.is_some_and(|p| p.contains_key(name)) {
                return Err(format!("required property {name:?} is not in `properties`"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn schema_checks() {
        assert!(check_schema(&json!({"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]})).is_ok());
        assert!(check_schema(&json!({"type": "string"})).is_err());
        assert!(
            check_schema(&json!({"type": "object", "required": ["a"]}))
                .unwrap_err()
                .contains("\"a\"")
        );
        assert!(check_schema(&json!({"type": "object", "properties": {"a": 1}})).is_err());
    }

    #[test]
    fn host_entries_must_be_hosts() {
        let ok = |h: &str| check_hosts(&[h.to_owned()]).is_ok();
        assert!(ok("api.linear.app") && ok("*.ghost.io") && ok("127.0.0.1") && ok("localhost"));
        for bad in [
            "*",
            "*.",
            "https://api.x.com",
            "api.x.com/v1",
            "x.com:443",
            "",
        ] {
            assert!(!ok(bad), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn idents() {
        assert!(valid_ident("linear") && valid_ident("create_issue2"));
        assert!(
            !valid_ident("Linear") && !valid_ident("2x") && !valid_ident("a-b") && !valid_ident("")
        );
    }
}
