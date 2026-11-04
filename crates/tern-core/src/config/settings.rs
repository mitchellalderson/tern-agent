//! The settings view of `config.toml` for the config editor: every scalar
//! setting, grouped by section, with its type, default, whether the file
//! sets it, a description from the shipped example's comments, and whether
//! it applies live. Built by walking [`Config::default`], so a new field
//! shows up without changes here; [`meta`] only adds what the defaults
//! can't say (choices, ranges, live, security notes).
//!
//! Providers, MCP servers and the Telegram gateway have their own editors
//! and hold secret names, so they're left out; secret values never pass
//! through here.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::edit::{ConfigFile, EditError};
use super::{Config, ModelRef};

/// Sections with editors of their own.
const ELSEWHERE: &[&str] = &["providers", "mcp", "gateway"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Bool,
    Int,
    Float,
    Text,
    Choice,
    /// A list of strings.
    List,
    /// `provider:model`, or a fallback chain of them.
    Model,
    /// Anything else (a table of aliases, say): shown, edited in the file.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Applies {
    /// From the next run or request.
    Live,
    /// When tern restarts.
    Restart,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Setting {
    /// Dotted path, e.g. `sandbox.network`.
    pub key: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// May be left unset (its default is `null`).
    pub optional: bool,
    /// What the file says, or the default.
    pub value: Value,
    pub default: Value,
    /// The file sets it (possibly to the default).
    pub set: bool,
    pub description: String,
    pub applies: Applies,
    /// Saved, but the running tern still uses the old value.
    pub restart_pending: bool,
    /// Why this setting matters for safety, for settings that do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<String>,
    /// The current value is the less safe choice.
    pub risky: bool,
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub settings: Vec<Setting>,
}

/// `GET /v1/config/settings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigView {
    pub path: String,
    /// The file doesn't load; sections are empty until it's fixed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub sections: Vec<Section>,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingError {
    #[error("unknown setting {0:?}")]
    Unknown(String),
    #[error("{0} is a secret; set it in Secrets")]
    Secret(String),
    #[error("{0} can't be changed here; edit config.toml")]
    NotEditable(String),
    #[error("{key}: {message}")]
    Invalid { key: String, message: String },
    #[error(transparent)]
    Edit(#[from] EditError),
}

/// What the defaults can't say about a setting.
#[derive(Clone, Copy)]
struct Meta {
    kind: Option<Kind>,
    choices: &'static [&'static str],
    min: Option<f64>,
    max: Option<f64>,
    live: bool,
    /// For top-level keys: the section to show them in.
    section: Option<&'static str>,
    security: Option<&'static str>,
    risky: fn(&Value) -> bool,
}

const META: Meta = Meta {
    kind: None,
    choices: &[],
    min: None,
    max: None,
    live: false,
    section: None,
    security: None,
    risky: |_| false,
};

const fn at_least(min: f64) -> Meta {
    Meta {
        min: Some(min),
        ..META
    }
}

fn meta(key: &str) -> Meta {
    let m = META;
    match key {
        "default_model" => Meta {
            kind: Some(Kind::Model),
            live: true,
            section: Some("models"),
            ..m
        },
        "context_window" => Meta {
            section: Some("models"),
            ..at_least(1000.0)
        },
        "max_iterations" | "secret_request_timeout_secs" => at_least(1.0),
        "sandbox.backend" => Meta {
            choices: &["host", "docker"],
            ..m
        },
        "sandbox.network" => Meta {
            security: Some(
                "On: shell commands can reach the internet, which also gets around the \
                 hosts a connector is allowed to reach.",
            ),
            risky: |v| v == &Value::Bool(true),
            ..m
        },
        "sandbox.confine" => Meta {
            choices: &["auto", "required", "off"],
            security: Some(
                "Off (or auto where the OS sandbox is missing): shell commands can read and \
                 write anywhere you can.",
            ),
            risky: |v| v == "off",
            ..m
        },
        "sandbox.read_paths" => Meta {
            security: Some("Every shell command can read these."),
            ..m
        },
        "sandbox.write_paths" => Meta {
            security: Some("Every shell command can change these."),
            ..m
        },
        "sandbox.shell_timeout_secs" => at_least(1.0),
        "sandbox.docker_cpus" | "sandbox.docker_pids" => at_least(0.0),
        "subagents.default_model" => Meta {
            kind: Some(Kind::Model),
            ..m
        },
        "subagents.max_depth" => Meta {
            min: Some(1.0),
            max: Some(3.0),
            ..m
        },
        "subagents.max_concurrent"
        | "subagents.max_children"
        | "subagents.max_concurrent_global"
        | "subagents.max_iterations"
        | "subagents.max_iterations_ceiling"
        | "subagents.ask_parent_timeout_secs" => at_least(1.0),
        "subagents.budget_tokens" | "subagents.budget_tokens_ceiling" => at_least(0.0),
        "schedules.quiet_hours" => Meta { live: true, ..m },
        "schedules.runs_per_day" | "schedules.tokens_per_run" => Meta {
            kind: Some(Kind::Int),
            live: true,
            ..at_least(1.0)
        },
        "schedules.usd_per_day" => Meta {
            kind: Some(Kind::Float),
            live: true,
            ..at_least(0.0)
        },
        "memory.user_md_tokens" | "memory.memory_md_tokens" => at_least(0.0),
        "home.idle_hours" => Meta {
            live: true,
            ..at_least(0.0)
        },
        "home.day_starts" => Meta { live: true, ..m },
        "api.listen" => Meta {
            security: Some("A tcp:// listener lets paired devices reach tern over the network."),
            risky: |v| v.as_array().is_some_and(|a| a.iter().any(|l| l != "unix")),
            ..m
        },
        "api.tls" => Meta {
            choices: &["auto", "files", "off"],
            security: Some(
                "Off sends device tokens and messages unencrypted: only behind a TLS proxy on \
                 this machine.",
            ),
            risky: |v| v == "off",
            ..m
        },
        "api.tls_cert" | "api.tls_key" | "api.public_url" => Meta {
            kind: Some(Kind::Text),
            ..m
        },
        "api.trust_proxy" => Meta {
            security: Some(
                "On: tern believes the client address a local proxy reports. Only with a \
                 reverse proxy in front.",
            ),
            risky: |v| v == &Value::Bool(true),
            ..m
        },
        "api.remote_mcp_stdio" => Meta {
            live: true,
            security: Some(
                "On: paired devices can add MCP servers that run commands on this computer, \
                 outside the sandbox.",
            ),
            risky: |v| v == &Value::Bool(true),
            ..m
        },
        _ => m,
    }
}

/// Display order; sections not listed go before `api`, in file order.
const SECTIONS: &[(&str, &str)] = &[
    ("general", "General"),
    ("models", "Models"),
    ("sandbox", "Shell & sandbox"),
    ("subagents", "Sub-agents"),
    ("schedules", "Schedules"),
    ("memory", "Memory"),
    ("home", "Home"),
];

fn title(id: &str) -> String {
    match SECTIONS.iter().find(|(s, _)| *s == id) {
        Some((_, t)) => (*t).to_owned(),
        None if id == "api" => "API & devices".into(),
        None => {
            let words = id.replace('_', " ");
            let mut c = words.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        }
    }
}

/// One setting the defaults define.
struct Field {
    key: String,
    section: String,
    default: Value,
}

/// Every setting [`Config::default`] has, in the order the example config
/// mentions them (others last, by name).
fn fields() -> Vec<Field> {
    let defaults = serde_json::to_value(Config::default()).unwrap_or_default();
    let mut out = Vec::new();
    walk(&defaults, "", &mut out);
    let order = &docs().order;
    out.sort_by_key(|f| {
        (
            order.get(&f.key).copied().unwrap_or(usize::MAX),
            f.key.clone(),
        )
    });
    out
}

fn walk(v: &Value, prefix: &str, out: &mut Vec<Field>) {
    let Some(map) = v.as_object() else { return };
    for (k, v) in map {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        if prefix.is_empty() && ELSEWHERE.contains(&k.as_str()) {
            continue;
        }
        match v {
            // A struct table; an empty one is a map (aliases), a leaf.
            Value::Object(m) if !m.is_empty() => walk(v, &key, out),
            _ => {
                let section = match key.split_once('.') {
                    Some((s, _)) => s.to_owned(),
                    None => meta(&key).section.unwrap_or("general").to_owned(),
                };
                out.push(Field {
                    key,
                    section,
                    default: v.clone(),
                });
            }
        }
    }
}

fn kind_of(meta: &Meta, default: &Value) -> Kind {
    if let Some(k) = meta.kind {
        return k;
    }
    if !meta.choices.is_empty() {
        return Kind::Choice;
    }
    match default {
        Value::Bool(_) => Kind::Bool,
        Value::Number(n) if n.is_f64() => Kind::Float,
        Value::Number(_) => Kind::Int,
        Value::String(_) => Kind::Text,
        Value::Array(a) if a.iter().all(Value::is_string) => Kind::List,
        _ => Kind::Other,
    }
}

fn lookup<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.').try_fold(v, |v, part| v.get(part))
}

fn is_secret_key(key: &str) -> bool {
    let leaf = key.rsplit('.').next().unwrap_or(key);
    leaf.ends_with("_secret") || leaf.ends_with("_secrets")
}

/// What the shipped `config.example.toml` says about each key and table
/// (commented-out keys count).
#[derive(Default)]
struct Docs {
    descriptions: HashMap<String, String>,
    /// Where each key is first mentioned.
    order: HashMap<String, usize>,
}

fn docs() -> &'static Docs {
    static D: OnceLock<Docs> = OnceLock::new();
    D.get_or_init(|| parse_docs(crate::defaults::CONFIG_TOML))
}

fn descriptions() -> &'static HashMap<String, String> {
    &docs().descriptions
}

/// A key's description is its inline comment, or else the comment block
/// just above it; a block above a `[table]` header describes the table
/// (stored under the table's name). Inline comments that only list the
/// choices are skipped. The first mention of a key wins.
fn parse_docs(text: &str) -> Docs {
    let mut docs = Docs::default();
    let out = &mut docs.descriptions;
    let mut table = String::new();
    let mut block: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let (body, commented) = match line.strip_prefix('#') {
            Some(rest) => (rest.trim(), true),
            None => (line, false),
        };
        if body.is_empty() {
            block.clear();
            continue;
        }
        if let Some(name) = body
            .strip_prefix('[')
            .and_then(|b| b.strip_suffix(']'))
            .filter(|n| n.chars().all(|c| c.is_alphanumeric() || "_.".contains(c)))
        {
            table = name.to_owned();
            if !block.is_empty() {
                out.entry(table.clone()).or_insert_with(|| block.join(" "));
            }
            block.clear();
            continue;
        }
        let key = body
            .split_once('=')
            .map(|(k, _)| k.trim())
            .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_alphanumeric() || c == '_'));
        match key {
            Some(k) => {
                let inline = inline_comment(body).filter(|c| !lists_choices(c));
                let desc = inline
                    .map(str::to_owned)
                    .or_else(|| (!block.is_empty()).then(|| block.join(" ")));
                let full = if table.is_empty() {
                    k.to_owned()
                } else {
                    format!("{table}.{k}")
                };
                let n = docs.order.len();
                docs.order.entry(full.clone()).or_insert(n);
                if let Some(d) = desc {
                    out.entry(full).or_insert(d);
                }
                block.clear();
            }
            None if commented => block.push(body.to_owned()),
            None => block.clear(),
        }
    }
    docs
}

/// The text after a `#` outside a string.
fn inline_comment(line: &str) -> Option<&str> {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return Some(line[i + 1..].trim()).filter(|c| !c.is_empty()),
            _ => {}
        }
    }
    None
}

/// `"host" | "docker"`: the UI shows those already.
fn lists_choices(comment: &str) -> bool {
    comment
        .split('|')
        .all(|p| p.trim().len() >= 2 && p.trim().starts_with('"') && p.trim().ends_with('"'))
}

/// The settings view of `file`, as `running` (what tern started with)
/// compares to it. A file that doesn't load gives an error and no
/// sections.
pub fn view(file: &ConfigFile, running: &Config) -> ConfigView {
    let path = file.path().display().to_string();
    let config = match file.parsed() {
        Ok(c) => c,
        Err(e) => {
            return ConfigView {
                path,
                error: Some(e.to_string()),
                sections: Vec::new(),
            };
        }
    };
    let current = serde_json::to_value(&config).unwrap_or_default();
    let running = serde_json::to_value(running).unwrap_or_default();
    let mut sections: Vec<Section> = Vec::new();
    for f in fields() {
        let s = setting(&f, file, &current, &running);
        match sections.iter_mut().find(|s| s.id == f.section) {
            Some(sec) => sec.settings.push(s),
            None => sections.push(Section {
                description: descriptions().get(&f.section).cloned().unwrap_or_default(),
                title: title(&f.section),
                id: f.section,
                settings: vec![s],
            }),
        }
    }
    let rank = |id: &str| match SECTIONS.iter().position(|(s, _)| *s == id) {
        Some(i) => i,
        None if id == "api" => usize::MAX,
        None => SECTIONS.len(),
    };
    sections.sort_by_key(|s| rank(&s.id));
    ConfigView {
        path,
        error: None,
        sections,
    }
}

fn setting(f: &Field, file: &ConfigFile, current: &Value, running: &Value) -> Setting {
    let m = meta(&f.key);
    let kind = kind_of(&m, &f.default);
    let value = lookup(current, &f.key).cloned().unwrap_or(Value::Null);
    let applies = if m.live {
        Applies::Live
    } else {
        Applies::Restart
    };
    Setting {
        kind,
        choices: m.choices.iter().map(|c| (*c).to_owned()).collect(),
        min: m.min,
        max: m.max,
        optional: f.default.is_null(),
        default: f.default.clone(),
        set: file.contains(&f.key),
        description: descriptions().get(&f.key).cloned().unwrap_or_default(),
        applies,
        restart_pending: applies == Applies::Restart
            && lookup(running, &f.key).unwrap_or(&Value::Null) != &value,
        security: m.security.map(str::to_owned),
        risky: (m.risky)(&value),
        editable: kind != Kind::Other && !is_secret_key(&f.key),
        value,
        key: f.key.clone(),
    }
}

fn field(key: &str) -> Result<Field, SettingError> {
    if is_secret_key(key) {
        return Err(SettingError::Secret(key.into()));
    }
    fields()
        .into_iter()
        .find(|f| f.key == key)
        .ok_or_else(|| SettingError::Unknown(key.into()))
}

/// Check `value` against `key`'s type and range, as the TOML to write.
/// `None` for `null`: unset it.
fn validate(key: &str, value: &Value) -> Result<Option<toml_edit::Value>, SettingError> {
    let f = field(key)?;
    let m = meta(key);
    let kind = kind_of(&m, &f.default);
    let invalid = |message: String| SettingError::Invalid {
        key: key.into(),
        message,
    };
    if kind == Kind::Other {
        return Err(SettingError::NotEditable(key.into()));
    }
    if value.is_null() {
        return Ok(None);
    }
    let range = |n: f64| -> Result<(), SettingError> {
        if let Some(min) = m.min.filter(|min| n < *min) {
            return Err(invalid(format!("must be at least {min}")));
        }
        if let Some(max) = m.max.filter(|max| n > *max) {
            return Err(invalid(format!("must be at most {max}")));
        }
        Ok(())
    };
    let strings = |v: &Value| -> Option<Vec<String>> {
        v.as_array()?
            .iter()
            .map(|x| x.as_str().map(str::to_owned))
            .collect()
    };
    let toml: toml_edit::Value = match kind {
        Kind::Bool => value
            .as_bool()
            .ok_or_else(|| invalid("expected true or false".into()))?
            .into(),
        Kind::Int => {
            let n = value
                .as_i64()
                .ok_or_else(|| invalid("expected a whole number".into()))?;
            range(n as f64)?;
            n.into()
        }
        Kind::Float => {
            let n = value
                .as_f64()
                .ok_or_else(|| invalid("expected a number".into()))?;
            range(n)?;
            n.into()
        }
        Kind::Text => value
            .as_str()
            .ok_or_else(|| invalid("expected text".into()))?
            .into(),
        Kind::Choice => {
            let s = value
                .as_str()
                .filter(|s| m.choices.contains(s))
                .ok_or_else(|| invalid(format!("must be one of {}", m.choices.join(", "))))?;
            s.into()
        }
        Kind::List => {
            let items = strings(value).ok_or_else(|| invalid("expected a list of text".into()))?;
            items
                .iter()
                .map(String::as_str)
                .collect::<toml_edit::Array>()
                .into()
        }
        Kind::Model => {
            let entries = match value {
                Value::String(s) => vec![s.clone()],
                v => strings(v).ok_or_else(|| invalid("expected provider:model".into()))?,
            };
            let chain = super::ModelChain::new(entries)
                .ok_or_else(|| invalid("a model list needs at least one entry".into()))?;
            for e in chain.entries() {
                e.parse::<ModelRef>().map_err(|e| invalid(e.to_string()))?;
            }
            chain.to_toml()
        }
        Kind::Other => unreachable!("refused above"),
    };
    Ok(Some(toml))
}

impl ConfigFile {
    /// Set a setting from its JSON value (`null` unsets it), checked for
    /// type and range. [`ConfigFile::save`] then checks the whole file.
    pub fn set_setting(&mut self, key: &str, value: &Value) -> Result<(), SettingError> {
        match validate(key, value)? {
            Some(v) => self.set_value(key, v)?,
            None => {
                self.remove_path(key);
            }
        }
        Ok(())
    }

    /// Back to the default: remove `key` from the file.
    pub fn reset_setting(&mut self, key: &str) -> Result<bool, SettingError> {
        field(key)?;
        Ok(self.remove_path(key))
    }
}

/// One setting of `view`.
pub fn find<'a>(view: &'a ConfigView, key: &str) -> Option<&'a Setting> {
    view.sections
        .iter()
        .flat_map(|s| &s.settings)
        .find(|s| s.key == key)
}

/// Settings keyed by path, for tests and the CLI.
pub fn by_key(view: &ConfigView) -> BTreeMap<&str, &Setting> {
    view.sections
        .iter()
        .flat_map(|s| &s.settings)
        .map(|s| (s.key.as_str(), s))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn file(dir: &tempfile::TempDir, body: &str) -> PathBuf {
        let p = dir.path().join("config.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn view_groups_types_and_marks_what_the_file_sets() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "[sandbox]\nnetwork = false\nconfine = \"off\"\n");
        let v = view(&ConfigFile::open(&path).unwrap(), &Config::default());
        assert!(v.error.is_none());
        let ids: Vec<&str> = v.sections.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "general",
                "models",
                "sandbox",
                "subagents",
                "schedules",
                "memory",
                "home",
                "outbox",
                "api"
            ]
        );
        let s = by_key(&v);
        assert!(!s.contains_key("providers") && !s.keys().any(|k| k.starts_with("mcp.")));
        let net = s["sandbox.network"];
        assert_eq!(net.kind, Kind::Bool);
        assert_eq!(
            (net.value.clone(), net.default.clone()),
            (json!(false), json!(true))
        );
        assert!(net.set && !net.risky && net.security.is_some());
        assert!(net.restart_pending, "running tern still has the default");
        let confine = s["sandbox.confine"];
        assert_eq!(confine.kind, Kind::Choice);
        assert_eq!(confine.choices, ["auto", "required", "off"]);
        assert!(confine.risky);
        assert!(
            confine.description.contains("OS sandbox"),
            "{}",
            confine.description
        );
        let model = s["default_model"];
        assert_eq!((model.kind, model.optional), (Kind::Model, true));
        assert_eq!(model.applies, Applies::Live);
        assert!(!model.set);
        assert_eq!(s["home.idle_hours"].kind, Kind::Float);
        assert_eq!(s["schedules.runs_per_day"].kind, Kind::Int);
        assert_eq!(s["sandbox.read_paths"].kind, Kind::List);
        let aliases = s["subagents.models"];
        assert_eq!(aliases.kind, Kind::Other);
        assert!(!aliases.editable);
        let general = &v.sections[0];
        assert_eq!(general.title, "General");
        assert!(general.settings.iter().any(|s| s.key == "workspace"));
        assert_eq!(v.sections[1].settings[0].key, "default_model");
    }

    #[test]
    fn every_setting_has_a_description() {
        let v = view(
            &ConfigFile::open(std::path::Path::new("/nonexistent/config.toml")).unwrap(),
            &Config::default(),
        );
        let missing: Vec<&str> = by_key(&v)
            .into_iter()
            .filter(|(_, s)| s.description.is_empty())
            .map(|(k, _)| k)
            .collect();
        assert!(
            missing.is_empty(),
            "describe these in config.example.toml: {missing:?}"
        );
    }

    #[test]
    fn descriptions_come_from_comments() {
        let docs = parse_docs(
            "# The model.\n# More.\ndefault_model = \"a:b\"\n\n[sandbox]\nbackend = \"host\" # \"host\" | \"docker\"\nnetwork = true  # false: no \"#\" networking\n\n# Memory things.\n# [memory]\n# flush = true # save first\n",
        );
        let d = &docs.descriptions;
        assert_eq!(d["default_model"], "The model. More.");
        assert!(!d.contains_key("sandbox.backend"));
        assert_eq!(d["sandbox.network"], "false: no \"#\" networking");
        assert_eq!(d["memory"], "Memory things.");
        assert_eq!(d["memory.flush"], "save first");
        assert!(docs.order["default_model"] < docs.order["memory.flush"]);
    }

    #[test]
    fn set_keeps_comments_and_reset_removes() {
        let dir = tempfile::tempdir().unwrap();
        let start = "# mine\n[sandbox]\n# why\nnetwork = true # inline\nbackend = \"host\"\n";
        let path = file(&dir, start);
        let mut f = ConfigFile::open(&path).unwrap();
        f.set_setting("sandbox.network", &json!(false)).unwrap();
        f.set_setting("subagents.max_depth", &json!(2)).unwrap();
        f.set_setting("default_model", &json!(["anthropic:a", "openrouter:b"]))
            .unwrap();
        f.set_setting("sandbox.write_paths", &json!(["~/.cache"]))
            .unwrap();
        let c = f.save().unwrap();
        assert!(!c.sandbox.network);
        assert_eq!(c.subagents.max_depth, 2);
        assert_eq!(c.default_model.unwrap().fallbacks(), ["openrouter:b"]);
        let text = std::fs::read_to_string(&path).unwrap();
        for kept in ["# mine", "# why", "# inline", "network = false"] {
            assert!(text.contains(kept), "{kept}: {text}");
        }

        let mut f = ConfigFile::open(&path).unwrap();
        assert!(f.reset_setting("sandbox.network").unwrap());
        assert!(!f.reset_setting("sandbox.network").unwrap());
        f.set_setting("default_model", &Value::Null).unwrap();
        let c = f.save().unwrap();
        assert!(c.sandbox.network);
        assert!(c.default_model.is_none());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# mine") && text.contains("backend"),
            "{text}"
        );
        assert!(!text.contains("network"), "{text}");
    }

    #[test]
    fn refuses_bad_values_and_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "");
        let mut f = ConfigFile::open(&path).unwrap();
        let err = |f: &mut ConfigFile, k: &str, v: Value| f.set_setting(k, &v).unwrap_err();
        assert!(matches!(
            err(&mut f, "sandbox.network", json!("yes")),
            SettingError::Invalid { .. }
        ));
        let e = err(&mut f, "subagents.max_depth", json!(9));
        assert!(e.to_string().contains("at most 3"), "{e}");
        assert!(matches!(
            err(&mut f, "max_iterations", json!(0)),
            SettingError::Invalid { .. }
        ));
        assert!(matches!(
            err(&mut f, "max_iterations", json!(1.5)),
            SettingError::Invalid { .. }
        ));
        let e = err(&mut f, "sandbox.confine", json!("sometimes"));
        assert!(e.to_string().contains("auto, required, off"), "{e}");
        assert!(matches!(
            err(&mut f, "sandbox.netwrk", json!(true)),
            SettingError::Unknown(_)
        ));
        assert!(matches!(
            err(&mut f, "providers.anthropic.kind", json!("x")),
            SettingError::Unknown(_)
        ));
        assert!(matches!(
            err(&mut f, "gateway.telegram.bot_token_secret", json!("x")),
            SettingError::Secret(_)
        ));
        assert!(matches!(
            err(&mut f, "subagents.models", json!({})),
            SettingError::NotEditable(_)
        ));
        assert!(matches!(
            err(&mut f, "default_model", json!("nocolon")),
            SettingError::Invalid { .. }
        ));
        // Passes the type check, fails the config's own parse.
        f.set_setting("schedules.quiet_hours", &json!("late"))
            .unwrap();
        assert!(f.save().is_err());
        assert!(std::fs::read_to_string(&path).unwrap().is_empty());
    }

    #[test]
    fn a_broken_file_reports_its_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, "max_iterations = \"lots\"\n");
        let v = view(&ConfigFile::open(&path).unwrap(), &Config::default());
        assert!(v.error.unwrap().contains("max_iterations"));
        assert!(v.sections.is_empty());
    }
}
