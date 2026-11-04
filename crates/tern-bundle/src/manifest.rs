//! `bundle.toml`: what a bundle is and the SHA-256 of every file in it
//! (tooling spec §6.5). The signature covers this file's exact bytes, so
//! it covers every file through their hashes.

use serde::{Deserialize, Serialize};

use crate::BundleError;

pub const MANIFEST: &str = "bundle.toml";
pub const SIGNATURE: &str = "bundle.toml.minisig";
pub const ROUTINES: &str = "routines.toml";
pub const README: &str = "README.md";

/// Limits on what a bundle may hold, so a hostile archive can't fill the
/// disk or memory.
pub const MAX_FILES: usize = 256;
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub bundle: Header,
    #[serde(default, rename = "file")]
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    /// `[a-z0-9][a-z0-9-]*`, at most 48: also the directory it is kept in.
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Free text: who made it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// The minisign public key `bundle.toml.minisig` was made with. No key,
    /// no signature: the bundle is unsigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
}

/// What a file in a bundle is, from its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// `connectors/<name>.luau`.
    Connector(String),
    /// `skills/<slug>/…`.
    Skill(String),
    /// `routines.toml`.
    Routines,
    /// `README.md`, shown at install.
    Readme,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, BundleError> {
        let m: Manifest = toml::from_str(text)
            .map_err(|e| BundleError::Invalid(format!("{MANIFEST}: {}", e.message())))?;
        m.check()?;
        Ok(m)
    }

    pub fn to_toml(&self) -> Result<String, BundleError> {
        toml::to_string(self).map_err(|e| BundleError::Invalid(e.to_string()))
    }

    fn check(&self) -> Result<(), BundleError> {
        check_name(&self.bundle.name)?;
        let v = self.bundle.version.trim();
        if v.is_empty() || v.len() > 32 || v.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(BundleError::Invalid(format!(
                "{MANIFEST}: version {:?} must be a short word like 1.2.0",
                self.bundle.version
            )));
        }
        if self.files.len() > MAX_FILES {
            return Err(BundleError::Invalid(format!(
                "{MANIFEST} lists {} files; at most {MAX_FILES}",
                self.files.len()
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for f in &self.files {
            kind(&f.path)?;
            if !seen.insert(f.path.as_str()) {
                return Err(BundleError::Invalid(format!(
                    "{MANIFEST} lists {} twice",
                    f.path
                )));
            }
            if f.sha256.len() != 64 || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(BundleError::Invalid(format!(
                    "{MANIFEST}: {} needs a sha256 of 64 hex digits",
                    f.path
                )));
            }
        }
        Ok(())
    }
}

/// A bundle's name: `[a-z0-9][a-z0-9-]*`, at most 48.
pub fn check_name(name: &str) -> Result<(), BundleError> {
    let ok = !name.is_empty()
        && name.len() <= 48
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(BundleError::Invalid(format!(
            "bundle name {name:?} must be lowercase letters, digits and dashes (at most 48)"
        )))
    }
}

/// Whether `s` is a skill directory name tern accepts.
pub fn valid_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The kind of the file at `path`, refusing paths a bundle may not hold:
/// anything outside the known layout, absolute paths, `..`, hidden files.
pub fn kind(path: &str) -> Result<Kind, BundleError> {
    let bad = |why: &str| BundleError::Invalid(format!("{path:?}: {why}"));
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.len() > 200
    {
        return Err(bad("not a plain relative path"));
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == ".." || p.starts_with('.'))
    {
        return Err(bad("no empty, `.`, `..` or hidden path parts"));
    }
    match parts.as_slice() {
        [ROUTINES] => Ok(Kind::Routines),
        [README] => Ok(Kind::Readme),
        ["connectors", file] => {
            let name = file
                .strip_suffix(".luau")
                .filter(|n| tern_lua::valid_ident(n))
                .ok_or_else(|| {
                    bad("connectors are connectors/<name>.luau, name in [a-z][a-z0-9_]*")
                })?;
            Ok(Kind::Connector(name.to_owned()))
        }
        ["skills", slug, _, ..] if valid_slug(slug) => Ok(Kind::Skill((*slug).to_owned())),
        ["skills", ..] => Err(bad(
            "skills are skills/<name>/SKILL.md (and files beside it)",
        )),
        ["agents", ..] => Err(bad(
            "agent profile templates can't be installed by this version of tern yet",
        )),
        _ => Err(bad(
            "a bundle holds connectors/*.luau, skills/<name>/…, routines.toml and README.md",
        )),
    }
}

/// A routine template (`routines.toml`, tooling spec §3.1's TOML form).
/// Bundles carry no agent: the routine belongs to the agent it's installed
/// for. No webhook triggers: those need a secret made on this machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutineTemplate {
    /// Unique in the bundle: how an update finds it again.
    pub name: String,
    pub prompt: String,
    pub trigger: TriggerTemplate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiative: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_hours: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgent: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<Budget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_connector: Option<String>,
    /// The spec's per-routine channel and model: accepted, shown, not
    /// applied (results go to the agent's usual place, with its model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerTemplate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_per_day: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd_per_day: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_per_run: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutinesFile {
    #[serde(default, rename = "routine")]
    pub routines: Vec<RoutineTemplate>,
}

impl TriggerTemplate {
    /// `cron 0 8 * * 1-5`, `once at …`, `watch github.prs every 5m`.
    pub fn describe(&self) -> String {
        match (&self.cron, &self.at, &self.watch) {
            (Some(c), _, _) => format!("cron {c}"),
            (_, Some(a), _) => format!("once at {a}"),
            (_, _, Some(w)) => format!(
                "watch {w} every {}",
                self.every.as_deref().unwrap_or("its default (5m)")
            ),
            _ => "no trigger".into(),
        }
    }
}

impl RoutinesFile {
    pub fn parse(text: &str) -> Result<Self, BundleError> {
        let f: RoutinesFile = toml::from_str(text)
            .map_err(|e| BundleError::Invalid(format!("{ROUTINES}: {}", e.message())))?;
        let mut names = std::collections::BTreeSet::new();
        for r in &f.routines {
            if r.name.trim().is_empty() || !names.insert(r.name.as_str()) {
                return Err(BundleError::Invalid(format!(
                    "{ROUTINES}: every routine needs a unique name ({:?})",
                    r.name
                )));
            }
            if r.prompt.trim().is_empty() {
                return Err(BundleError::Invalid(format!(
                    "{ROUTINES}: routine {} has no prompt",
                    r.name
                )));
            }
            let t = &r.trigger;
            let n = [t.cron.is_some(), t.at.is_some(), t.watch.is_some()]
                .iter()
                .filter(|b| **b)
                .count();
            if n != 1 {
                return Err(BundleError::Invalid(format!(
                    "{ROUTINES}: routine {} needs exactly one of trigger.cron, trigger.at or trigger.watch (webhooks can't ride in a bundle)",
                    r.name
                )));
            }
            if t.every.is_some() && t.watch.is_none() {
                return Err(BundleError::Invalid(format!(
                    "{ROUTINES}: routine {}: `every` is for watches",
                    r.name
                )));
            }
        }
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(
            kind("connectors/notion.luau").unwrap(),
            Kind::Connector("notion".into())
        );
        assert_eq!(
            kind("skills/brief/SKILL.md").unwrap(),
            Kind::Skill("brief".into())
        );
        assert_eq!(
            kind("skills/brief/scripts/a.sh").unwrap(),
            Kind::Skill("brief".into())
        );
        assert_eq!(kind("routines.toml").unwrap(), Kind::Routines);
        for bad in [
            "../x",
            "/etc/passwd",
            "connectors/../x.luau",
            "connectors/Notion.luau",
            "connectors/a/b.luau",
            "skills/x",
            "skills/.hidden/SKILL.md",
            "skills/a/.env",
            "bin/tern",
            "agents/research.toml",
            "a\\b",
        ] {
            assert!(kind(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn routines_need_one_trigger() {
        let ok = RoutinesFile::parse(
            "[[routine]]\nname = \"a\"\nprompt = \"p\"\ntrigger = { cron = \"0 8 * * *\" }\n",
        )
        .unwrap();
        assert_eq!(ok.routines[0].trigger.describe(), "cron 0 8 * * *");
        assert!(
            RoutinesFile::parse("[[routine]]\nname = \"a\"\nprompt = \"p\"\ntrigger = {}\n")
                .is_err()
        );
        assert!(
            RoutinesFile::parse(
                "[[routine]]\nname = \"a\"\nprompt = \"p\"\ntrigger = { cron = \"x\" }\nagent_id = \"y\"\n"
            )
            .is_err(),
            "unknown keys (an agent id) are refused"
        );
    }
}
