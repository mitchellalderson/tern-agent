//! `tern bundle export`: package your own connectors, skills and routines
//! to share (tooling spec §6.5). Secrets never go in: connectors carry
//! secret *names*, routines carry no webhook secret or agent, hidden files
//! and key-like files in skills are left out, and every file is checked
//! for the value of every secret tern knows before anything is written.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use tern_core::{Secrets, TernHome};
use tern_store::{ScheduleRow, Store};

use crate::manifest::{
    AGENT_FILE, AgentTemplate, AvatarTemplate, Budget, Header, MAX_FILE_BYTES, PERSONA_FILE,
    ROUTINES, RoutineTemplate, RoutinesFile, TriggerTemplate, valid_slug,
};
use crate::sign::SecretKey;
use crate::source::{assemble, pack};
use crate::state::Lock;
use crate::{BundleError, Paths};

/// What to export. Empty lists with `all` take everything of yours.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub header: Header,
    pub connectors: Vec<String>,
    pub skills: Vec<String>,
    /// Routine ids (`sch_…`).
    pub routines: Vec<String>,
    /// Agents (slugs) to include as templates: persona, tool scope, memory
    /// scope, avatar and autonomy ceiling. Routines of an exported agent
    /// go in as that agent's. Never the default agent, its model, its
    /// Telegram bot or its memory.
    pub agents: Vec<String>,
    /// Every active connector, skill and routine (of `agent`) that didn't
    /// come from an installed bundle.
    pub all: bool,
    /// Whose routines `all` takes; default tern.
    pub agent: Option<String>,
}

pub struct Exported {
    /// The bundle's files, `bundle.toml` (and its signature) included.
    pub files: BTreeMap<String, Vec<u8>>,
    /// What was left out, and why.
    pub notes: Vec<String>,
    pub signed_by: Option<String>,
}

impl Exported {
    /// Write as a `.tar.zst`; returns its SHA-256 (what others pin).
    pub fn write_archive(&self, path: &Path) -> Result<String, BundleError> {
        let bytes = pack(&self.files)?;
        std::fs::write(path, &bytes)
            .map_err(|e| BundleError::Io(format!("{}: {e}", path.display())))?;
        Ok(crate::sign::sha256_hex(&bytes))
    }

    /// Write as a directory (for a git repository); returns the SHA-256 of
    /// `bundle.toml`, which pins it.
    pub fn write_dir(&self, dir: &Path) -> Result<String, BundleError> {
        if dir.exists()
            && std::fs::read_dir(dir)
                .map_err(|e| BundleError::Io(e.to_string()))?
                .next()
                .is_some()
        {
            return Err(BundleError::Refused(format!(
                "{} isn't empty",
                dir.display()
            )));
        }
        for (p, bytes) in &self.files {
            let target = dir.join(p);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| BundleError::Io(e.to_string()))?;
            }
            std::fs::write(&target, bytes).map_err(|e| BundleError::Io(e.to_string()))?;
        }
        Ok(crate::sign::sha256_hex(
            &self.files[crate::manifest::MANIFEST],
        ))
    }
}

pub fn export(
    home: &TernHome,
    store: Option<&Store>,
    req: &Request,
    key: Option<&SecretKey>,
) -> Result<Exported, BundleError> {
    crate::manifest::check_name(&req.header.name)?;
    let lock = Lock::load(&Paths::new(home).lock())?;
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut notes = Vec::new();

    // Connectors: the active file, else a draft.
    let mut connectors: BTreeSet<String> = req.connectors.iter().cloned().collect();
    if req.all {
        for p in luau_files(&home.connectors_dir()) {
            let name = stem(&p);
            if lock.owner_of_connector(&name).is_none() {
                connectors.insert(name);
            }
        }
    }
    for name in &connectors {
        if !tern_lua::valid_ident(name) {
            return Err(BundleError::Invalid(format!("no connector named {name:?}")));
        }
        let active = home.connectors_dir().join(format!("{name}.luau"));
        let draft = home.drafts_dir().join(format!("{name}.luau"));
        let path = if active.is_file() {
            active
        } else if draft.is_file() {
            notes.push(format!(
                "connector {name}: exported the draft (it isn't active)"
            ));
            draft
        } else {
            return Err(BundleError::Invalid(format!("no connector named {name}")));
        };
        let bytes = std::fs::read(&path).map_err(|e| BundleError::Io(e.to_string()))?;
        files.insert(format!("connectors/{name}.luau"), bytes);
    }

    // Skills: the directory, without hidden or key-like files.
    let mut skills: BTreeSet<String> = req.skills.iter().cloned().collect();
    if req.all {
        for s in tern_core::skills::discover(&home.skills_dir()) {
            let slug = s
                .dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // tern's own skills, unless edited, aren't yours to share.
            let builtin = tern_core::defaults::SKILLS.iter().any(|(dir, text)| {
                *dir == slug
                    && std::fs::read_to_string(s.dir.join("SKILL.md")).is_ok_and(|t| t == *text)
            });
            if lock.owner_of_skill(&slug).is_none() && !builtin {
                skills.insert(slug);
            }
        }
    }
    for slug in &skills {
        let dir = home.skills_dir().join(slug);
        if !valid_slug(slug) || !dir.join("SKILL.md").is_file() {
            return Err(BundleError::Invalid(format!("no skill in skills/{slug}")));
        }
        for (rel, bytes) in skill_files(&dir, &mut notes, slug)? {
            files.insert(format!("skills/{slug}/{rel}"), bytes);
        }
    }

    // Agents, as templates.
    let mut agent_slugs: BTreeMap<String, String> = BTreeMap::new(); // id -> slug
    if !req.agents.is_empty() {
        let store =
            store.ok_or_else(|| BundleError::Invalid("agents need tern's database".into()))?;
        for want in &req.agents {
            let a = store
                .agent(want.trim().trim_start_matches('@'))
                .map_err(|e| BundleError::Io(e.to_string()))?
                .ok_or_else(|| BundleError::Invalid(format!("no agent {want:?}")))?;
            if a.is_default() {
                return Err(BundleError::Invalid(
                    "the default agent isn't exported: its persona and memory are yours".into(),
                ));
            }
            if lock
                .bundles
                .iter()
                .any(|b| b.agents.iter().any(|x| x.id == a.id))
            {
                notes.push(format!(
                    "agent @{}: it came from an installed bundle; exported anyway as you asked",
                    a.slug
                ));
            }
            let persona = std::fs::read_to_string(tern_core::agents::persona_path(home, &a.id))
                .map_err(|e| BundleError::Io(format!("reading @{}'s persona: {e}", a.slug)))?;
            let tools = a
                .tools
                .as_deref()
                .map(tern_core::agents::ToolScope::parse)
                .transpose()
                .map_err(|e| BundleError::Invalid(format!("@{}'s tools: {e}", a.slug)))?;
            let t = AgentTemplate {
                name: a.name.clone(),
                description: String::new(),
                avatar: a
                    .avatar
                    .as_deref()
                    .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok())
                    .and_then(|v| {
                        Some(AvatarTemplate {
                            glyph: v.get("glyph")?.as_str()?.to_owned(),
                            color: v.get("color")?.as_str()?.to_owned(),
                        })
                    }),
                memory: Some(a.memory.clone()),
                tools,
                autonomy: a.autonomy.clone(),
            };
            files.insert(
                format!("agents/{}/{AGENT_FILE}", a.slug),
                toml::to_string(&t)
                    .map_err(|e| BundleError::Io(e.to_string()))?
                    .into_bytes(),
            );
            files.insert(
                format!("agents/{}/{PERSONA_FILE}", a.slug),
                persona.into_bytes(),
            );
            agent_slugs.insert(a.id.clone(), a.slug.clone());
        }
    }

    // Routines, as templates.
    let mut routines: Vec<ScheduleRow> = Vec::new();
    if !req.routines.is_empty() || req.all || !agent_slugs.is_empty() {
        let store =
            store.ok_or_else(|| BundleError::Invalid("routines need tern's database".into()))?;
        let all = store
            .schedules()
            .map_err(|e| BundleError::Io(e.to_string()))?;
        let from_bundles: BTreeSet<&str> = lock
            .bundles
            .iter()
            .flat_map(|b| b.routines.iter().map(|r| r.id.as_str()))
            .collect();
        for id in &req.routines {
            let r = all
                .iter()
                .find(|r| &r.id == id)
                .ok_or_else(|| BundleError::Invalid(format!("no routine {id}")))?;
            routines.push(r.clone());
        }
        // An exported agent's routines come with it.
        for r in &all {
            if agent_slugs.contains_key(r.agent())
                && !from_bundles.contains(r.id.as_str())
                && !routines.iter().any(|x| x.id == r.id)
            {
                routines.push(r.clone());
            }
        }
        if req.all {
            let agent = req.agent.as_deref().unwrap_or(tern_store::DEFAULT_AGENT);
            for r in &all {
                if r.agent() == agent
                    && !from_bundles.contains(r.id.as_str())
                    && !routines.iter().any(|x| x.id == r.id)
                {
                    routines.push(r.clone());
                }
            }
        }
    }
    let mut templates = Vec::new();
    let mut names = BTreeSet::new();
    for r in &routines {
        match template(r, &mut names) {
            Ok(mut t) => {
                t.agent = agent_slugs.get(r.agent()).cloned();
                templates.push(t);
            }
            Err(why) if req.all || agent_slugs.contains_key(r.agent()) => {
                notes.push(format!("routine {}: left out: {why}", r.id))
            }
            Err(why) => return Err(BundleError::Invalid(format!("routine {}: {why}", r.id))),
        }
    }
    if !templates.is_empty() {
        let text = toml::to_string(&RoutinesFile {
            routines: templates,
        })
        .map_err(|e| BundleError::Io(e.to_string()))?;
        files.insert(ROUTINES.into(), text.into_bytes());
    }

    if files.is_empty() {
        return Err(BundleError::Invalid(
            "nothing to export: name connectors, skills, agents or routines, or pass --all".into(),
        ));
    }

    // Never a secret value, wherever it hides.
    let secrets =
        Secrets::load(&home.secrets_path()).map_err(|e| BundleError::Io(e.to_string()))?;
    check_no_secrets(&secrets, &files)?;

    let files = assemble(req.header.clone(), files, key)?;
    Ok(Exported {
        files,
        notes,
        signed_by: key.map(|k| k.public.id_hex()),
    })
}

/// Refuse if any file holds the value of a secret tern knows.
pub fn check_no_secrets(
    secrets: &Secrets,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), BundleError> {
    let named: Vec<(String, String)> = secrets
        .file_names()
        .into_iter()
        .filter_map(|n| secrets.get(&n).map(|v| (n, v)))
        .filter(|(_, v)| v.len() >= 6)
        .collect();
    for (path, bytes) in files {
        let text = String::from_utf8_lossy(bytes);
        if let Some((name, _)) = named.iter().find(|(_, v)| text.contains(v.as_str())) {
            return Err(BundleError::Refused(format!(
                "{path} contains the value of secret {name}; take it out (use capabilities.auth) and export again"
            )));
        }
    }
    Ok(())
}

/// Files that look like credentials, never exported from a skill.
fn key_like(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.ends_with(".pem")
        || n.ends_with(".key")
        || n.ends_with(".p12")
        || n.ends_with(".pfx")
        || n.starts_with("id_rsa")
        || n.starts_with("id_ed25519")
        || n.contains("credential")
        || n.contains("secret")
        || n == "token.json"
}

pub(crate) fn skill_files(
    dir: &Path,
    notes: &mut Vec<String>,
    slug: &str,
) -> Result<Vec<(String, Vec<u8>)>, BundleError> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(dir.join(&rel))
            .map_err(|e| BundleError::Io(e.to_string()))?
            .flatten()
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let rel_path = rel.join(&name);
            let shown = rel_path.to_string_lossy().replace('\\', "/");
            let Ok(ft) = e.file_type() else { continue };
            if name.starts_with('.') || ft.is_symlink() {
                notes.push(format!(
                    "skill {slug}: left out {shown} (hidden file or link)"
                ));
                continue;
            }
            if ft.is_dir() {
                stack.push(rel_path);
                continue;
            }
            if key_like(&name) {
                notes.push(format!(
                    "skill {slug}: left out {shown} (looks like a credential)"
                ));
                continue;
            }
            let bytes = std::fs::read(e.path()).map_err(|e| BundleError::Io(e.to_string()))?;
            if bytes.len() > MAX_FILE_BYTES {
                notes.push(format!("skill {slug}: left out {shown} (larger than 1 MB)"));
                continue;
            }
            out.push((shown, bytes));
        }
    }
    Ok(out)
}

/// A routine as a template: trigger, prompt, initiative and limits. Its
/// agent, channel, delivery target and session stay here.
fn template(r: &ScheduleRow, names: &mut BTreeSet<String>) -> Result<RoutineTemplate, String> {
    let trigger = match r.trigger() {
        "cron" => TriggerTemplate {
            cron: Some(r.cron.clone()),
            ..TriggerTemplate::default()
        },
        "watch" => TriggerTemplate {
            watch: r.watch.clone(),
            every: r.every_secs.map(tern_tools::schedule::fmt_every),
            ..TriggerTemplate::default()
        },
        "once" => {
            if r.next_run_at == i64::MAX {
                return Err("it already ran".into());
            }
            let at = chrono::DateTime::from_timestamp_millis(r.next_run_at)
                .ok_or("its time is out of range")?
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string();
            TriggerTemplate {
                at: Some(at),
                ..TriggerTemplate::default()
            }
        }
        "webhook" => return Err("webhook routines need a secret made where they run".into()),
        other => return Err(format!("unknown trigger {other}")),
    };
    let base = name_for(&r.prompt);
    let mut name = base.clone();
    let mut n = 2;
    while !names.insert(name.clone()) {
        name = format!("{base}-{n}");
        n += 1;
    }
    let budget = Budget {
        runs_per_day: r.runs_per_day,
        usd_per_day: r.usd_per_day,
        tokens_per_run: r.tokens_per_run,
    };
    Ok(RoutineTemplate {
        name,
        agent: None,
        prompt: r.prompt.clone(),
        trigger,
        initiative: r.initiative.clone(),
        quiet_hours: r.quiet_hours.clone(),
        urgent: r.urgent.then_some(true),
        autonomy: r.autonomy.clone(),
        budget: (budget != Budget::default()).then_some(budget),
        gate: r.gate.clone(),
        gate_connector: r.gate_connector.clone(),
        channel: None,
        model: None,
    })
}

/// `Summarize my unread mail every morning` → `summarize-my-unread-mail`.
fn name_for(prompt: &str) -> String {
    let words: Vec<String> = prompt
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(4)
        .map(str::to_ascii_lowercase)
        .collect();
    if words.is_empty() {
        "routine".into()
    } else {
        words.join("-")
    }
}

fn luau_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "luau"))
        .collect();
    v.sort();
    v
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}
