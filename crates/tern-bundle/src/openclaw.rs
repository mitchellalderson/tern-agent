//! `tern import openclaw`: bring an OpenClaw setup across (tooling spec
//! §6.5). What maps cleanly becomes an unsigned bundle installed through
//! the same plan and approval as any other: SKILL.md skills (the same
//! agentskills format) and cron automations as paused routines. SOUL.md,
//! USER.md and MEMORY.md go to tern's persona, user and memory files.
//! Everything else is reported, not converted: plugins (Node code),
//! channels, MCP servers, model providers, heartbeat, and above all
//! credentials, which are never read into tern: the report names the
//! secrets and accounts to add.
//!
//! Layout as documented at docs.openclaw.ai on 2026-11-04: a state
//! directory (`~/.openclaw`: `openclaw.json` in JSON5, `credentials/`,
//! managed `skills/`) and a workspace (`~/.openclaw/workspace`: `SOUL.md`,
//! `USER.md`, `IDENTITY.md`, `AGENTS.md`, `MEMORY.md`, `memory/`,
//! `skills/`). Current releases keep automations in SQLite; tern reads the
//! pre-2026.9.7 `cron/jobs.json` and the JSON `openclaw automations get`
//! prints, saved as `automations.json` in either directory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::Value;
use tern_core::TernHome;

use crate::BundleError;
use crate::manifest::{
    Header, ROUTINES, RoutineTemplate, RoutinesFile, TriggerTemplate, valid_slug,
};
use crate::source::{Origin, Raw, assemble};

/// The bundle an import installs as; importing again updates it.
pub const BUNDLE_NAME: &str = "openclaw-import";

#[derive(Debug, Default)]
pub struct Import {
    pub state_dir: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
    /// Skills and routines, as a bundle's files (no manifest yet).
    pub files: BTreeMap<String, Vec<u8>>,
    /// Persona, user and memory files to write.
    pub profile: Vec<ProfileWrite>,
    /// Secret names something imported needs: (name, what for).
    pub secrets: Vec<(String, String)>,
    /// What to set up by hand, with how.
    pub setup: Vec<String>,
    /// What wasn't converted, and why.
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ProfileWrite {
    pub from: String,
    pub to: PathBuf,
    /// `write` (the tern file is missing or still the default), `append`
    /// (under a heading), or `beside` (a new file next to it).
    pub how: String,
    pub text: String,
}

/// The default OpenClaw state directory.
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".openclaw"))
}

/// Look at an OpenClaw state directory or workspace and work out what
/// would be imported. Reads only; writes nothing.
pub fn scan(dir: &Path, home: &TernHome) -> Result<Import, BundleError> {
    if !dir.is_dir() {
        return Err(BundleError::Invalid(format!(
            "{} isn't a directory",
            dir.display()
        )));
    }
    let dir = &std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut imp = Import::default();
    let (state, workspace) = locate(dir);
    let config = match &state {
        Some(s) => read_config(&s.join("openclaw.json"), &mut imp.skipped),
        None => Value::Null,
    };
    let workspace = config_workspace(&config, state.as_deref())
        .and_then(|w| std::fs::canonicalize(w).ok())
        .filter(|w| w.is_dir())
        .or(workspace);
    if state.is_none() && workspace.is_none() {
        return Err(BundleError::Invalid(format!(
            "{} doesn't look like OpenClaw's directory (~/.openclaw, with openclaw.json) or its workspace (with SOUL.md or AGENTS.md)",
            dir.display()
        )));
    }
    imp.state_dir.clone_from(&state);
    imp.workspace.clone_from(&workspace);
    let creds = credential_values(&config);

    // Skills: the workspace's win over managed ones of the same name.
    let mut seen = BTreeSet::new();
    let mut sources: Vec<(PathBuf, &str)> = Vec::new();
    if let Some(w) = &workspace {
        sources.push((w.join("skills"), "workspace"));
    }
    if let Some(s) = &state {
        sources.push((s.join("skills"), "managed (ClawHub or installed by hand)"));
    }
    for (root, origin) in sources {
        for dir in subdirs(&root) {
            if !dir.join("SKILL.md").is_file() {
                continue;
            }
            let raw = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let slug = slugify(&raw);
            if !seen.insert(slug.clone()) {
                continue;
            }
            import_skill(&mut imp, &dir, &slug, origin, &creds)?;
        }
    }
    skill_config_secrets(&mut imp, &config);

    // Automations.
    let mut jobs = Vec::new();
    let mut job_files: Vec<PathBuf> = Vec::new();
    for d in state.iter().chain(workspace.iter()) {
        job_files.push(d.join("cron").join("jobs.json"));
        job_files.push(d.join("automations.json"));
    }
    if let Some(store) = config.pointer("/cron/store").and_then(Value::as_str) {
        job_files.push(expand(store, state.as_deref()));
    }
    job_files.dedup();
    for f in job_files.iter().filter(|f| f.is_file()) {
        match std::fs::read_to_string(f)
            .ok()
            .and_then(|t| parse_json5(&t).ok())
        {
            Some(v) => jobs.extend(job_list(v)),
            None => imp
                .skipped
                .push(format!("{}: not JSON tern can read", f.display())),
        }
    }
    let mut templates = Vec::new();
    let mut names = BTreeSet::new();
    for job in &jobs {
        match routine(job, &mut names) {
            Ok((t, note)) => {
                if let Some(n) = note {
                    imp.skipped.push(n);
                }
                templates.push(t);
            }
            Err(why) => imp.skipped.push(why),
        }
    }
    if let Some(s) = &state
        && jobs.is_empty()
    {
        imp.setup.push(format!(
            "automations: OpenClaw 2026.9.7 and later keep them in its database. Save each with `openclaw automations get <id>` into {} and import again",
            s.join("automations.json").display()
        ));
    }
    if !templates.is_empty() {
        let text = toml::to_string(&RoutinesFile {
            routines: templates,
        })
        .map_err(|e| BundleError::Io(e.to_string()))?;
        imp.files.insert(ROUTINES.into(), text.into_bytes());
    }

    if let Some(w) = &workspace {
        profile(&mut imp, w, home);
        for (file, why) in [
            (
                "AGENTS.md",
                "operating instructions written for OpenClaw's tools; copy what still applies into persona.md by hand",
            ),
            (
                "BOOT.md",
                "a startup checklist for OpenClaw's boot hook; tern has none",
            ),
            ("BOOTSTRAP.md", "OpenClaw's first-run ritual"),
            ("TOOLS.md", "notes about OpenClaw's tools"),
            (
                "HEARTBEAT.md",
                "the heartbeat checklist: make a routine for it if you want one (tell tern what to check, and when)",
            ),
        ] {
            if w.join(file).is_file() {
                imp.skipped.push(format!("{file}: {why}"));
            }
        }
        if w.join("memory").is_dir() {
            imp.skipped.push(
                "memory/*.md daily logs: not imported (MEMORY.md was); tern's own memory starts fresh".into(),
            );
        }
    }
    report_config(&mut imp, &config);
    if let Some(s) = &state
        && s.join("credentials").is_dir()
    {
        imp.skipped.push(
            "credentials/: never imported. Sign in again with `tern connections add`, or set the secrets listed".into(),
        );
    }
    // One line per secret, with every reason.
    let mut merged: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, why) in std::mem::take(&mut imp.secrets) {
        let reasons = merged.entry(name).or_default();
        if !reasons.contains(&why) {
            reasons.push(why);
        }
    }
    imp.secrets = merged.into_iter().map(|(n, w)| (n, w.join("; "))).collect();
    Ok(imp)
}

/// The import as a bundle's raw files, ready for [`crate::source::verify`].
pub fn as_raw(imp: &Import, dir: &Path) -> Result<Option<Raw>, BundleError> {
    if imp.files.is_empty() {
        return Ok(None);
    }
    let header = Header {
        name: BUNDLE_NAME.into(),
        version: chrono::Local::now().format("%Y.%m.%d").to_string(),
        description: "Skills and automations imported from OpenClaw".into(),
        author: "tern import openclaw".into(),
        key: None,
    };
    let files = assemble(header, imp.files.clone(), None)?;
    let sha256 = crate::sign::sha256_hex(&files[crate::manifest::MANIFEST]);
    Ok(Some(Raw {
        origin: Origin::Generated(format!("openclaw:{}", dir.display())),
        sha256,
        files,
    }))
}

/// Write the persona, user and memory files. Text already there isn't
/// written twice, so importing again is harmless.
pub fn apply_profile(imp: &Import) -> Result<Vec<String>, BundleError> {
    let mut out = Vec::new();
    for p in &imp.profile {
        let current = std::fs::read_to_string(&p.to).unwrap_or_default();
        if current.contains(p.text.trim()) {
            out.push(format!("{}: already in {}", p.from, p.to.display()));
            continue;
        }
        if let Some(parent) = p.to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| BundleError::Io(e.to_string()))?;
        }
        let text = match p.how.as_str() {
            "append" => format!(
                "{}\n\n## Imported from OpenClaw ({})\n\n{}\n",
                current.trim_end(),
                p.from,
                p.text.trim()
            ),
            _ => format!("{}\n", p.text.trim()),
        };
        std::fs::write(&p.to, text)
            .map_err(|e| BundleError::Io(format!("{}: {e}", p.to.display())))?;
        out.push(format!("{} → {} ({})", p.from, p.to.display(), p.how));
    }
    Ok(out)
}

impl Import {
    /// What isn't part of the bundle's plan: profile files, needs, skips.
    pub fn render(&self) -> String {
        let mut o = String::new();
        if let Some(s) = &self.state_dir {
            o.push_str(&format!("OpenClaw state: {}\n", s.display()));
        }
        if let Some(w) = &self.workspace {
            o.push_str(&format!("OpenClaw workspace: {}\n", w.display()));
        }
        if !self.profile.is_empty() {
            o.push_str("\nProfile files:\n");
            for p in &self.profile {
                let how = match p.how.as_str() {
                    "write" => "becomes",
                    "append" => "is added to the end of",
                    _ => "is saved beside yours as",
                };
                o.push_str(&format!("  {} {how} {}\n", p.from, p.to.display()));
            }
        }
        if !self.secrets.is_empty() {
            o.push_str("\nSecrets to add yourself (values are never imported):\n");
            for (name, why) in &self.secrets {
                o.push_str(&format!("  {name} — {why}\n"));
            }
        }
        if !self.setup.is_empty() {
            o.push_str("\nSet up in tern:\n");
            for s in &self.setup {
                o.push_str(&format!("  {s}\n"));
            }
        }
        if !self.skipped.is_empty() {
            o.push_str("\nNot converted:\n");
            for s in &self.skipped {
                o.push_str(&format!("  {s}\n"));
            }
        }
        o
    }
}

/// (state dir, workspace) for a directory that is either.
fn locate(dir: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let is_workspace = |d: &Path| {
        d.join("SOUL.md").is_file() || d.join("AGENTS.md").is_file() || d.join("USER.md").is_file()
    };
    if dir.join("openclaw.json").is_file() {
        let ws = dir.join("workspace");
        return (Some(dir.to_path_buf()), ws.is_dir().then_some(ws));
    }
    if is_workspace(dir) {
        let state = dir
            .parent()
            .filter(|p| p.join("openclaw.json").is_file())
            .map(Path::to_path_buf);
        return (state, Some(dir.to_path_buf()));
    }
    let ws = dir.join("workspace");
    (None, (ws.is_dir() && is_workspace(&ws)).then_some(ws))
}

fn read_config(path: &Path, skipped: &mut Vec<String>) -> Value {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Value::Null;
    };
    match parse_json5(&text) {
        Ok(v) => v,
        Err(e) => {
            skipped.push(format!(
                "openclaw.json: couldn't read it ({e}); skills and files were still imported"
            ));
            Value::Null
        }
    }
}

fn config_workspace(config: &Value, state: Option<&Path>) -> Option<PathBuf> {
    let w = config.pointer("/agents/defaults/workspace")?.as_str()?;
    Some(expand(w, state))
}

fn expand(p: &str, base: Option<&Path>) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/")
        && let Some(h) = std::env::var_os("HOME")
    {
        return PathBuf::from(h).join(rest);
    }
    match base {
        Some(b) if Path::new(p).is_relative() => b.join(p),
        _ => PathBuf::from(p),
    }
}

fn subdirs(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !p.is_symlink())
        .collect();
    v.sort();
    v
}

fn slugify(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let out = out.trim_matches('-').chars().take(64).collect::<String>();
    if valid_slug(&out) {
        out
    } else {
        "skill".into()
    }
}

/// Every string in the config under a key that names a credential: these
/// values must never end up in an imported file.
fn credential_values(config: &Value) -> Vec<String> {
    fn walk(v: &Value, key: &str, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                for (k, v) in m {
                    walk(v, k, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|v| walk(v, key, out)),
            Value::String(s) if s.len() >= 6 && is_credential_key(key) => out.push(s.clone()),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(config, "", &mut out);
    out
}

fn is_credential_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    ["key", "token", "secret", "password", "bearer", "credential"]
        .iter()
        .any(|w| k.contains(w))
}

fn import_skill(
    imp: &mut Import,
    dir: &Path,
    slug: &str,
    origin: &str,
    creds: &[String],
) -> Result<(), BundleError> {
    let text = std::fs::read_to_string(dir.join("SKILL.md")).unwrap_or_default();
    let Some((name, _, _)) = tern_core::skills::parse(&text) else {
        imp.skipped.push(format!(
            "skill {slug}: its SKILL.md has no `name` and `description` frontmatter"
        ));
        return Ok(());
    };
    let mut notes = Vec::new();
    let files = crate::export::skill_files(dir, &mut notes, slug)?;
    imp.skipped.extend(notes);
    for (rel, bytes) in &files {
        let body = String::from_utf8_lossy(bytes);
        if creds.iter().any(|c| body.contains(c.as_str())) {
            imp.skipped.push(format!(
                "skill {slug}: not imported, because {rel} holds a credential from openclaw.json"
            ));
            return Ok(());
        }
    }
    for (rel, bytes) in files {
        imp.files.insert(format!("skills/{slug}/{rel}"), bytes);
    }
    if origin.starts_with("managed") {
        let named = if name == slug {
            String::new()
        } else {
            format!(" ({name})")
        };
        imp.setup.push(format!(
            "skill {slug}{named} came from OpenClaw's managed skills (ClawHub or installed by hand): read it before you rely on it"
        ));
    }
    // OpenClaw's metadata: `metadata: {"openclaw": {"requires": {"env": […], "bins": […]}}}`.
    let meta = frontmatter_value(&text, "metadata")
        .and_then(|m| parse_json5(&m).ok())
        .unwrap_or(Value::Null);
    let inner = ["openclaw", "clawdbot", "moltbot"]
        .iter()
        .find_map(|k| meta.get(*k))
        .unwrap_or(&meta);
    for env in inner
        .pointer("/requires/env")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        imp.secrets
            .push((env.to_owned(), format!("skill {slug} uses it")));
    }
    let bins: Vec<&str> = inner
        .pointer("/requires/bins")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !bins.is_empty() {
        imp.setup.push(format!(
            "skill {slug} runs {} through the shell: install {}, and allow {} in the sandbox if it's on",
            bins.join(", "),
            if bins.len() == 1 { "it" } else { "them" },
            if bins.len() == 1 { "it" } else { "them" }
        ));
    }
    Ok(())
}

/// `key: value` from SKILL.md's frontmatter, raw.
fn frontmatter_value(text: &str, key: &str) -> Option<String> {
    let rest = text.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    rest[..end].lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim().to_owned())
    })
}

/// `skills.entries.<name>.env` / `apiKey`: names to set, never values.
fn skill_config_secrets(imp: &mut Import, config: &Value) {
    let Some(entries) = config.pointer("/skills/entries").and_then(Value::as_object) else {
        return;
    };
    for (skill, e) in entries {
        if let Some(env) = e.get("env").and_then(Value::as_object) {
            for name in env.keys() {
                imp.secrets.push((
                    name.clone(),
                    format!("skill {skill} (set in openclaw.json)"),
                ));
            }
        }
        if e.get("apiKey").is_some() {
            let name = e
                .get("primaryEnv")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!("{}_API_KEY", skill.to_ascii_uppercase().replace('-', "_"))
                });
            imp.secrets.push((
                name,
                format!("skill {skill}'s API key (set in openclaw.json)"),
            ));
        }
    }
}

fn job_list(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        Value::Object(ref m) if m.contains_key("jobs") => m
            .get("jobs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        Value::Object(ref m) if m.contains_key("job") => {
            m.get("job").cloned().into_iter().collect()
        }
        Value::Object(ref m) if m.contains_key("schedule") => vec![v],
        _ => Vec::new(),
    }
}

/// An automation as a routine template, or why it can't be one.
fn routine(
    job: &Value,
    names: &mut BTreeSet<String>,
) -> Result<(RoutineTemplate, Option<String>), String> {
    let label = job
        .get("name")
        .or_else(|| job.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("an automation")
        .to_owned();
    let what = format!("automation {label:?}");
    let s = job.get("schedule").ok_or(format!("{what}: no schedule"))?;
    let kind = s.get("kind").and_then(Value::as_str).unwrap_or_default();
    let mut note = None;
    let trigger = match kind {
        "cron" => {
            let expr = s
                .get("expr")
                .or_else(|| s.get("cron"))
                .and_then(Value::as_str)
                .ok_or(format!("{what}: a cron schedule with no expression"))?;
            expr.parse::<tern_core::cron::Cron>()
                .map_err(|e| format!("{what}: its cron {expr:?} isn't one tern reads ({e})"))?;
            if let Some(tz) = s.get("tz").and_then(Value::as_str) {
                note = Some(format!(
                    "{what}: ran in {tz}; tern runs it at the same clock time in this computer's time zone"
                ));
            }
            TriggerTemplate {
                cron: Some(expr.to_owned()),
                ..TriggerTemplate::default()
            }
        }
        "every" => {
            let ms = s
                .get("everyMs")
                .and_then(Value::as_i64)
                .or_else(|| {
                    s.get("every")
                        .and_then(Value::as_str)
                        .and_then(|e| tern_tools::schedule::parse_every(e).ok().map(|x| x * 1000))
                })
                .ok_or(format!("{what}: an interval with no length"))?;
            let cron = every_as_cron(ms).ok_or(format!(
                "{what}: every {} doesn't fit a clock schedule; make a routine for it with a cron time",
                tern_tools::schedule::fmt_every(ms / 1000)
            ))?;
            if ms == 86_400_000 {
                note = Some(format!(
                    "{what}: ran once a day; it runs at 09:00 now (change it after installing)"
                ));
            }
            TriggerTemplate {
                cron: Some(cron),
                ..TriggerTemplate::default()
            }
        }
        "at" => {
            let at = match (s.get("at"), s.get("atMs")) {
                (Some(Value::String(a)), _) => chrono::DateTime::parse_from_rfc3339(a)
                    .map(|t| t.with_timezone(&chrono::Local))
                    .or_else(|_| tern_tools::schedule::parse_at(a))
                    .map_err(|_| format!("{what}: its time {a:?} isn't one tern reads"))?,
                (_, Some(ms)) => ms
                    .as_i64()
                    .and_then(chrono::DateTime::from_timestamp_millis)
                    .map(|t| t.with_timezone(&chrono::Local))
                    .ok_or(format!("{what}: its time isn't one tern reads"))?,
                _ => return Err(format!("{what}: a one-off with no time")),
            };
            if at <= chrono::Local::now() {
                return Err(format!("{what}: a one-off whose time has passed"));
            }
            TriggerTemplate {
                at: Some(at.format("%Y-%m-%d %H:%M").to_string()),
                ..TriggerTemplate::default()
            }
        }
        "stream" => {
            return Err(format!(
                "{what}: it watches a command's output; tern's watches are connector functions, so ask tern to write one"
            ));
        }
        other => {
            return Err(format!(
                "{what}: schedule kind {other:?} isn't one tern knows"
            ));
        }
    };
    let payload = job.get("payload").unwrap_or(&Value::Null);
    let prompt = payload
        .get("message")
        .or_else(|| payload.get("text"))
        .or_else(|| job.get("message"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .ok_or(format!("{what}: no message to run"))?
        .to_owned();
    // Delivered to a chat (the usual reminder): push it; otherwise the
    // conservative default, the digest.
    let delivers = match job.get("delivery") {
        Some(Value::Object(d)) => d.get("mode").and_then(Value::as_str) != Some("none"),
        Some(Value::Bool(b)) => *b,
        _ => job.get("deliver").and_then(Value::as_bool).unwrap_or(false),
    };
    let base = slugify(&label);
    let mut name = base.clone();
    let mut n = 2;
    while !names.insert(name.clone()) {
        name = format!("{base}-{n}");
        n += 1;
    }
    Ok((
        RoutineTemplate {
            name,
            prompt,
            trigger,
            initiative: delivers.then(|| "interrupt".to_owned()),
            quiet_hours: None,
            urgent: None,
            autonomy: None,
            budget: None,
            gate: None,
            gate_connector: None,
            channel: None,
            model: None,
        },
        note,
    ))
}

/// An interval that a cron line says exactly: minutes dividing an hour,
/// hours dividing a day, or a day.
fn every_as_cron(ms: i64) -> Option<String> {
    let mins = ms / 60_000;
    if ms % 60_000 != 0 || mins < 1 {
        return None;
    }
    match mins {
        m if m < 60 && 60 % m == 0 => Some(format!("*/{m} * * * *")),
        60 => Some("0 * * * *".into()),
        m if m % 60 == 0 && m < 1440 && 24 % (m / 60) == 0 => Some(format!("0 */{} * * *", m / 60)),
        1440 => Some("0 9 * * *".into()),
        _ => None,
    }
}

fn profile(imp: &mut Import, ws: &Path, home: &TernHome) {
    let read = |f: &str| {
        std::fs::read_to_string(ws.join(f))
            .ok()
            .filter(|t| !t.trim().is_empty())
    };
    let soul = read("SOUL.md");
    let identity = read("IDENTITY.md");
    if soul.is_some() || identity.is_some() {
        let text = [identity, soul]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n\n");
        let target = home.persona_path();
        let current = std::fs::read_to_string(&target).unwrap_or_default();
        let fresh = current.trim().is_empty() || current == tern_core::defaults::PERSONA_MD;
        // Imported before: apply_profile sees it's there and skips it.
        let fresh = fresh || current.contains(text.trim());
        let (to, how) = if fresh {
            (target, "write")
        } else {
            (home.root().join("persona.openclaw.md"), "beside")
        };
        imp.profile.push(ProfileWrite {
            from: "SOUL.md and IDENTITY.md".into(),
            to,
            how: how.into(),
            text,
        });
        if !fresh {
            imp.setup.push(
                "persona: yours was kept; OpenClaw's is in persona.openclaw.md to merge by hand"
                    .into(),
            );
        }
    }
    for (file, target, default) in [
        ("USER.md", home.user_path(), tern_core::defaults::USER_MD),
        ("MEMORY.md", home.memory_path(), ""),
    ] {
        let Some(text) = read(file) else { continue };
        let current = std::fs::read_to_string(&target).unwrap_or_default();
        let how = if current.trim().is_empty() || current == default {
            "write"
        } else {
            "append"
        };
        imp.profile.push(ProfileWrite {
            from: file.into(),
            to: target,
            how: how.into(),
            text,
        });
    }
}

fn report_config(imp: &mut Import, config: &Value) {
    let keys = |p: &str| -> Vec<String> {
        config
            .pointer(p)
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    };
    for p in keys("/plugins/entries") {
        imp.skipped.push(format!(
            "plugin {p}: OpenClaw plugins are Node code and aren't converted. If it talks to a web API, ask tern to write a connector for it"
        ));
    }
    for c in keys("/channels") {
        let how = match c.as_str() {
            "telegram" => "enable [gateway.telegram] and set its bot token secret (not imported), then `tern gateway pair`".to_owned(),
            "slack" => "`tern gateway add slack`".to_owned(),
            "ntfy" => "`tern gateway add ntfy`".to_owned(),
            other => format!("{other} isn't a tern gateway yet"),
        };
        imp.setup.push(format!("channel {c}: {how}"));
    }
    let mut mcp = keys("/mcp/servers");
    mcp.extend(keys("/mcpServers"));
    for name in mcp {
        let def = config
            .pointer(&format!("/mcp/servers/{name}"))
            .or_else(|| config.pointer(&format!("/mcpServers/{name}")))
            .unwrap_or(&Value::Null);
        let how = match (
            def.get("url").and_then(Value::as_str),
            def.get("command").and_then(Value::as_str),
        ) {
            (Some(u), _) => format!("tern mcp add {name} --url {u}"),
            (None, Some(c)) => {
                let args: Vec<&str> = def
                    .get("args")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                format!("tern mcp add {name} -- {c} {}", args.join(" "))
            }
            _ => "add it with `tern mcp add`".into(),
        };
        imp.setup
            .push(format!("MCP server {name}: {}", how.trim_end()));
    }
    if config.pointer("/models/providers").is_some() || config.pointer("/auth/profiles").is_some() {
        imp.setup
            .push("model providers: run `tern setup` (keys and sign-ins aren't imported)".into());
    }
    let heartbeat_file = imp.skipped.iter().any(|s| s.starts_with("HEARTBEAT.md"));
    if config.pointer("/agents/defaults/heartbeat").is_some() && !heartbeat_file {
        imp.skipped.push(
            "heartbeat: make a routine for it if you want one (a cron time and what to check)"
                .into(),
        );
    }
    let agents = keys("/agents/entries");
    if agents.len() > 1 {
        imp.skipped.push(format!(
            "agents {}: only the default workspace was imported; make the others with `tern agents new`",
            agents.join(", ")
        ));
    }
}

/// Parse JSON5 as OpenClaw writes it (comments, trailing commas, unquoted
/// keys, single quotes) by rewriting it as JSON.
pub fn parse_json5(text: &str) -> Result<Value, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                // Past the `*`; the `/` is skipped below.
                i += 1;
            }
            '"' | '\'' => {
                let quote = c;
                out.push('"');
                i += 1;
                while i < chars.len() && chars[i] != quote {
                    match chars[i] {
                        '\\' if i + 1 < chars.len() => {
                            if chars[i + 1] == '\'' {
                                out.push('\'');
                            } else {
                                out.push('\\');
                                out.push(chars[i + 1]);
                            }
                            i += 2;
                            continue;
                        }
                        '"' => out.push_str("\\\""),
                        '\n' => out.push_str("\\n"),
                        ch => out.push(ch),
                    }
                    i += 1;
                }
                // The closing quote is skipped below.
                out.push('"');
            }
            ',' => {
                // A trailing comma: the next meaningful character closes.
                let mut j = i + 1;
                loop {
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if chars.get(j) == Some(&'/')
                        && matches!(chars.get(j + 1), Some('/') | Some('*'))
                    {
                        if chars[j + 1] == '/' {
                            while j < chars.len() && chars[j] != '\n' {
                                j += 1;
                            }
                        } else {
                            j += 2;
                            while j + 1 < chars.len() && !(chars[j] == '*' && chars[j + 1] == '/') {
                                j += 1;
                            }
                            j += 2;
                        }
                        continue;
                    }
                    break;
                }
                if !matches!(chars.get(j), Some('}') | Some(']')) {
                    out.push(',');
                }
            }
            c if c.is_alphabetic() || c == '_' || c == '$' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
                {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                let mut j = i;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if chars.get(j) == Some(&':') {
                    out.push('"');
                    out.push_str(&word);
                    out.push('"');
                } else {
                    out.push_str(match word.as_str() {
                        "Infinity" | "NaN" => "null",
                        w => w,
                    });
                }
                continue;
            }
            '+' => {}
            c => out.push(c),
        }
        i += 1;
    }
    serde_json::from_str(&out).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json5_as_openclaw_writes_it() {
        let v = parse_json5(
            "// config\n{\n  agents: { defaults: { workspace: '~/.openclaw/workspace', }, },\n  /* block */\n  list: [1, 2,],\n  'quoted': \"a // not a comment\",\n  ok: true,\n}\n",
        )
        .unwrap();
        assert_eq!(
            v["agents"]["defaults"]["workspace"],
            "~/.openclaw/workspace"
        );
        assert_eq!(v["list"], serde_json::json!([1, 2]));
        assert_eq!(v["quoted"], "a // not a comment");
        assert_eq!(v["ok"], true);
        let v = parse_json5("{a: /* x */ 'it\\'s', b: \"q\\\"q\", c: [1 /* y */, 2]}").unwrap();
        assert_eq!(v["a"], "it's");
        assert_eq!(v["b"], "q\"q");
        assert_eq!(v["c"], serde_json::json!([1, 2]));
    }

    #[test]
    fn intervals_that_fit_a_clock() {
        assert_eq!(every_as_cron(15 * 60_000).as_deref(), Some("*/15 * * * *"));
        assert_eq!(every_as_cron(3_600_000).as_deref(), Some("0 * * * *"));
        assert_eq!(every_as_cron(6 * 3_600_000).as_deref(), Some("0 */6 * * *"));
        assert_eq!(every_as_cron(7 * 60_000), None);
        assert_eq!(every_as_cron(5 * 3_600_000), None);
    }
}
