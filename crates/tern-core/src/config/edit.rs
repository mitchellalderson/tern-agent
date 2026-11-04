//! Editing `config.toml` in place, keeping comments and layout. Every save
//! is validated with [`Config::parse`] and written atomically, so a bad
//! edit never replaces a working file.

use std::path::{Path, PathBuf};

use super::{Config, ConfigError, McpServerConfig, ProviderConfig, ProviderKind};

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("parsing {path}: {message}")]
    Toml { path: PathBuf, message: String },
    #[error("invalid key {0:?}")]
    Key(String),
    #[error("{part:?} in {key:?} is not a table")]
    NotTable { part: String, key: String },
    #[error("writing {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// `config.toml` opened for editing.
pub struct ConfigFile {
    path: PathBuf,
    doc: toml_edit::DocumentMut,
}

impl ConfigFile {
    /// Open `path`; a missing file starts empty.
    pub fn open(path: &Path) -> Result<Self, EditError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                }
                .into());
            }
        };
        let doc = text
            .parse()
            .map_err(|e: toml_edit::TomlError| EditError::Toml {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;
        Ok(Self {
            path: path.to_path_buf(),
            doc,
        })
    }

    /// Set a dotted `key` (`sandbox.backend`) to `raw`, parsed as a TOML
    /// value when it is one and taken as a string otherwise.
    pub fn set(&mut self, key: &str, raw: &str) -> Result<(), EditError> {
        let value: toml_edit::Value = raw.parse().unwrap_or_else(|_| raw.into());
        self.set_value(key, value)
    }

    pub fn set_value(&mut self, key: &str, value: toml_edit::Value) -> Result<(), EditError> {
        let parts: Vec<&str> = key.split('.').collect();
        let Some((leaf, tables)) = parts.split_last() else {
            return Err(EditError::Key(key.into()));
        };
        if parts.iter().any(|p| p.is_empty()) {
            return Err(EditError::Key(key.into()));
        }
        let mut table = self.doc.as_table_mut();
        for part in tables {
            let entry = table
                .entry(part)
                .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
            table = entry.as_table_mut().ok_or_else(|| EditError::NotTable {
                part: (*part).into(),
                key: key.into(),
            })?;
        }
        set_keeping_comments(table, leaf, value);
        Ok(())
    }

    /// Add or replace `[providers.<name>]`. Comments inside a replaced
    /// table are kept; only its fields change.
    pub fn upsert_provider(&mut self, name: &str, p: &ProviderConfig) -> Result<(), EditError> {
        let providers = self
            .doc
            .entry("providers")
            .or_insert_with(|| {
                let mut t = toml_edit::Table::new();
                t.set_implicit(true);
                toml_edit::Item::Table(t)
            })
            .as_table_mut()
            .ok_or_else(|| EditError::NotTable {
                part: "providers".into(),
                key: format!("providers.{name}"),
            })?;
        let table = providers
            .entry(name)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| EditError::NotTable {
                part: name.into(),
                key: format!("providers.{name}"),
            })?;
        let kind = match p.kind {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenaiCompat => "openai_compat",
        };
        set_keeping_comments(table, "kind", kind.into());
        set_opt(table, "base_url", p.base_url.clone().map(Into::into));
        set_opt(
            table,
            "api_key_secret",
            p.api_key_secret.clone().map(Into::into),
        );
        set_opt(
            table,
            "context_window",
            p.context_window
                .and_then(|w| i64::try_from(w).ok())
                .map(Into::into),
        );
        Ok(())
    }

    /// Remove `[providers.<name>]`. Returns whether it existed.
    pub fn remove_provider(&mut self, name: &str) -> bool {
        self.doc
            .get_mut("providers")
            .and_then(|p| p.as_table_mut())
            .is_some_and(|t| t.remove(name).is_some())
    }

    /// Add or replace `[mcp.servers.<name>]`. Fields the config no longer
    /// sets are removed; comments on the ones that stay are kept.
    pub fn upsert_mcp_server(
        &mut self,
        name: &str,
        cfg: &McpServerConfig,
    ) -> Result<(), EditError> {
        self.upsert_table(&["mcp", "servers"], name, cfg)
    }

    /// Add or replace `[gateway.connectors.<name>]`, like
    /// [`ConfigFile::upsert_mcp_server`].
    pub fn upsert_gateway_connector(
        &mut self,
        name: &str,
        cfg: &super::GatewayConnectorConfig,
    ) -> Result<(), EditError> {
        self.upsert_table(&["gateway", "connectors"], name, cfg)
    }

    /// Add or replace `[<parents…>.<name>]` with `cfg`'s fields.
    fn upsert_table(
        &mut self,
        parents: &[&str],
        name: &str,
        cfg: &impl serde::Serialize,
    ) -> Result<(), EditError> {
        let key = format!("{}.{name}", parents.join("."));
        let not_table = |part: &str| EditError::NotTable {
            part: part.into(),
            key: key.clone(),
        };
        let mut table = self.doc.as_table_mut();
        for part in parents.iter().copied() {
            table = table
                .entry(part)
                .or_insert_with(|| {
                    let mut t = toml_edit::Table::new();
                    t.set_implicit(true);
                    toml_edit::Item::Table(t)
                })
                .as_table_mut()
                .ok_or_else(|| not_table(part))?;
        }
        let table = table
            .entry(name)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| not_table(name))?;
        let fields = match serde_json::to_value(cfg) {
            Ok(serde_json::Value::Object(m)) => m,
            _ => return Err(EditError::Key(key)),
        };
        let stale: Vec<String> = table
            .iter()
            .map(|(k, _)| k.to_owned())
            .filter(|k| !fields.contains_key(k))
            .collect();
        for k in stale {
            table.remove(&k);
        }
        for (k, v) in fields {
            let v = to_toml(&v).ok_or_else(|| EditError::Key(format!("{key}.{k}")))?;
            set_keeping_comments(table, &k, v);
        }
        Ok(())
    }

    /// Remove `[mcp.servers.<name>]`. Returns whether it existed.
    pub fn remove_mcp_server(&mut self, name: &str) -> bool {
        self.doc
            .get_mut("mcp")
            .and_then(|m| m.get_mut("servers"))
            .and_then(|s| s.as_table_like_mut())
            .is_some_and(|t| t.remove(name).is_some())
    }

    /// Remove a top-level key such as `default_model`.
    pub fn remove(&mut self, key: &str) -> bool {
        self.doc.remove(key).is_some()
    }

    /// Remove a dotted `key` (`sandbox.network`), keeping its table even
    /// when that leaves it empty. Returns whether it was set.
    pub fn remove_path(&mut self, key: &str) -> bool {
        let Some((tables, leaf)) = key.rsplit_once('.') else {
            return self.remove(key);
        };
        let mut item = self.doc.as_item_mut();
        for part in tables.split('.') {
            match item.get_mut(part) {
                Some(i) => item = i,
                None => return false,
            }
        }
        item.as_table_like_mut()
            .is_some_and(|t| t.remove(leaf).is_some())
    }

    /// Whether the file sets the dotted `key`.
    pub fn contains(&self, key: &str) -> bool {
        key.split('.')
            .try_fold(self.doc.as_item(), |item, part| item.get(part))
            .is_some()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The config this file would load as.
    pub fn parsed(&self) -> Result<Config, ConfigError> {
        Config::parse(&self.doc.to_string(), &self.path)
    }

    /// Validate, then write atomically. Returns the saved config.
    pub fn save(&self) -> Result<Config, EditError> {
        let body = self.doc.to_string();
        let config = Config::parse(&body, &self.path)?;
        let write = |p: &Path, f: &dyn Fn() -> std::io::Result<()>| {
            f().map_err(|source| EditError::Write {
                path: p.to_path_buf(),
                source,
            })
        };
        if let Some(dir) = self.path.parent() {
            write(dir, &|| std::fs::create_dir_all(dir))?;
        }
        let tmp = self.path.with_extension("toml.tmp");
        write(&tmp, &|| std::fs::write(&tmp, &body))?;
        write(&self.path, &|| std::fs::rename(&tmp, &self.path))?;
        Ok(config)
    }
}

/// A JSON value as TOML: objects become inline tables. `None` for `null`.
fn to_toml(v: &serde_json::Value) -> Option<toml_edit::Value> {
    use serde_json::Value as J;
    Some(match v {
        J::Null => return None,
        J::Bool(b) => (*b).into(),
        J::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n.as_f64()?.into(),
        },
        J::String(s) => s.as_str().into(),
        J::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for x in a {
                arr.push(to_toml(x)?);
            }
            arr.into()
        }
        J::Object(m) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, x) in m {
                t.insert(k, to_toml(x)?);
            }
            t.into()
        }
    })
}

/// Replace `key`'s value but keep the comments around it.
fn set_keeping_comments(table: &mut toml_edit::Table, key: &str, mut value: toml_edit::Value) {
    match table.get_mut(key).and_then(|i| i.as_value_mut()) {
        Some(old) => {
            *value.decor_mut() = old.decor().clone();
            *old = value;
        }
        None => {
            table.insert(key, toml_edit::value(value));
        }
    }
}

fn set_opt(table: &mut toml_edit::Table, key: &str, value: Option<toml_edit::Value>) {
    match value {
        Some(v) => set_keeping_comments(table, key, v),
        None => {
            table.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: &str = "\
# my config
default_model = \"anthropic:old\"

# keep me
[providers.anthropic]
kind = \"anthropic\" # inline
api_key_secret = \"ANTHROPIC_API_KEY\"
";

    fn file(dir: &tempfile::TempDir, body: &str) -> PathBuf {
        let p = dir.path().join("config.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn upserts_and_keeps_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, START);
        let mut f = ConfigFile::open(&path).unwrap();
        f.upsert_provider(
            "ollama",
            &ProviderConfig {
                kind: ProviderKind::OpenaiCompat,
                base_url: Some("http://localhost:11434/v1".into()),
                api_key_secret: None,
                context_window: None,
                prices: Default::default(),
            },
        )
        .unwrap();
        f.set("default_model", "ollama:llama3").unwrap();
        let c = f.save().unwrap();
        assert_eq!(c.default_model.as_deref(), Some("ollama:llama3"));
        assert_eq!(c.providers.len(), 2);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my config"));
        assert!(text.contains("# keep me"));
        assert!(text.contains("# inline"));
        assert!(text.contains("[providers.ollama]"));
    }

    #[test]
    fn replaces_fields_of_an_existing_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, START);
        let mut f = ConfigFile::open(&path).unwrap();
        f.upsert_provider(
            "anthropic",
            &ProviderConfig {
                kind: ProviderKind::Anthropic,
                base_url: Some("https://proxy.example.com".into()),
                api_key_secret: Some("OTHER_KEY".into()),
                context_window: None,
                prices: Default::default(),
            },
        )
        .unwrap();
        let c = f.save().unwrap();
        let p = &c.providers["anthropic"];
        assert_eq!(p.api_key_secret.as_deref(), Some("OTHER_KEY"));
        assert_eq!(p.base_url.as_deref(), Some("https://proxy.example.com"));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("# keep me")
        );
    }

    #[test]
    fn removes_and_starts_from_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, START);
        let mut f = ConfigFile::open(&path).unwrap();
        assert!(f.remove_provider("anthropic"));
        assert!(!f.remove_provider("anthropic"));
        assert!(f.remove("default_model"));
        let c = f.save().unwrap();
        assert!(c.providers.is_empty());
        assert_eq!(c.default_model, None);

        let fresh = dir.path().join("sub/new.toml");
        let mut f = ConfigFile::open(&fresh).unwrap();
        f.set("max_iterations", "5").unwrap();
        assert_eq!(f.save().unwrap().max_iterations, 5);
    }

    #[test]
    fn writes_and_removes_mcp_servers() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(
            &dir,
            "[mcp.servers.github]\n# the token\nenv_secrets = [\"GITHUB_TOKEN\"]\ntransport = \"stdio\"\ncommand = \"old\"\nidempotent = true\n",
        );
        let mut f = ConfigFile::open(&path).unwrap();
        let mut cfg = McpServerConfig {
            enabled: true,
            command: Some("github-mcp-server".into()),
            args: vec!["stdio".into()],
            env_secrets: vec!["GITHUB_TOKEN".into()],
            ..Default::default()
        };
        cfg.env.insert("LOG_LEVEL".into(), "warn".into());
        f.upsert_mcp_server("github", &cfg).unwrap();
        let other = McpServerConfig {
            transport: super::super::McpTransport::Http,
            enabled: false,
            url: Some("https://mcp.example.com/mcp".into()),
            bearer_token_secret: Some("EXAMPLE_TOKEN".into()),
            ..Default::default()
        };
        f.upsert_mcp_server("example", &other).unwrap();
        let c = f.save().unwrap();
        assert_eq!(c.mcp.servers["github"], cfg);
        assert_eq!(c.mcp.servers["example"], other);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# the token"), "{text}");
        assert!(!text.contains("idempotent"), "{text}");

        let mut f = ConfigFile::open(&path).unwrap();
        assert!(f.remove_mcp_server("github"));
        assert!(!f.remove_mcp_server("github"));
        let c = f.save().unwrap();
        assert_eq!(c.mcp.servers.keys().collect::<Vec<_>>(), ["example"]);
    }

    #[test]
    fn refuses_invalid_configs() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(&dir, START);
        let mut f = ConfigFile::open(&path).unwrap();
        f.set("max_iterations", "\"lots\"").unwrap();
        assert!(matches!(f.save(), Err(EditError::Config(_))));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), START);
        assert!(matches!(f.set("a..b", "1"), Err(EditError::Key(_))));
        assert!(matches!(
            f.set("default_model.x", "1"),
            Err(EditError::NotTable { .. })
        ));
    }
}
