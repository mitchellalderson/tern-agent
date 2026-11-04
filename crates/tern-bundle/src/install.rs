//! Installing, updating and removing bundles. Nothing here asks the user:
//! callers show [`Prepared::plan`] and call [`Installer::install`] only
//! after approval. What is installed can't do anything yet: connectors
//! land in `connectors/drafts/` and are activated one by one through the
//! normal activation approval; routines are created paused.

use std::collections::BTreeMap;
use std::path::Path;

use tern_core::config::Rate;
use tern_core::{Config, Secrets, TernHome};
use tern_store::Store;
use tern_tools::schedule::{ScheduleLimits, TriggerArgs};

use crate::manifest::{
    AGENT_FILE, AgentTemplate, Kind, PERSONA_FILE, ROUTINES, RoutineTemplate, RoutinesFile, kind,
};
use crate::plan::{Context, Plan, plan, scratch_host};
use crate::source::{self, Bundle, Fetcher, Signed};
use crate::state::{InstalledAgent, InstalledBundle, InstalledRoutine, Lock, TrustStore, now};
use crate::{BundleError, Paths};

pub struct Installer {
    pub home: TernHome,
    pub paths: Paths,
    pub fetcher: Fetcher,
    pub default_rate: Rate,
    /// `[bundles] require_signed`.
    pub require_signed: bool,
}

/// A checked bundle and what installing it would do.
pub struct Prepared {
    pub bundle: Bundle,
    pub plan: Plan,
}

#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// The agent (id, @name or name) its routines belong to; default tern.
    pub agent: Option<String>,
    /// Accept a signing key other than the one the installed version was
    /// signed with (or an unsigned update of a signed bundle).
    pub accept_new_key: bool,
}

/// What was done, line by line.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Outcome {
    pub name: String,
    pub lines: Vec<String>,
}

impl Installer {
    pub fn new(home: &TernHome) -> Self {
        let config = Config::load(&home.config_path())
            .map(|(c, _)| c)
            .unwrap_or_default();
        Self {
            home: home.clone(),
            paths: Paths::new(home),
            fetcher: Fetcher {
                allow_private: config.bundles.allow_private_hosts,
            },
            default_rate: config.connectors.rate,
            require_signed: config.bundles.require_signed,
        }
    }

    /// Read and check a bundle from a URL (`…#sha256=<hex>` pins it) or a
    /// path. `pin` pins it too; a URL and `pin` must agree.
    pub async fn read(&self, source: &str, pin: Option<&str>) -> Result<Bundle, BundleError> {
        let (src, url_pin) = source::split_pin(source);
        let pin = match (url_pin, pin) {
            (Some(a), Some(b))
                if a != b.trim().trim_start_matches("sha256:").to_ascii_lowercase() =>
            {
                return Err(BundleError::Invalid(
                    "the URL's #sha256= and --sha256 differ".into(),
                ));
            }
            (a, b) => a.or_else(|| b.map(str::to_owned)),
        };
        let raw = if source::is_url(&src) {
            self.fetcher.fetch(&src).await?
        } else {
            source::read_path(Path::new(&src))?
        };
        source::verify(raw, pin.as_deref())
    }

    /// The plan for `bundle` against what is installed now.
    pub fn prepare(&self, bundle: Bundle, store: Option<&Store>) -> Result<Prepared, BundleError> {
        let secrets =
            Secrets::load(&self.home.secrets_path()).map_err(|e| BundleError::Io(e.to_string()))?;
        let host = scratch_host(
            Secrets::load(&self.home.secrets_path()).map_err(|e| BundleError::Io(e.to_string()))?,
            self.default_rate,
        )?;
        let lock = Lock::load(&self.paths.lock())?;
        let trust = TrustStore::load(&self.paths.trusted())?;
        let cx = Context {
            home: &self.home,
            host: &host,
            secrets: &secrets,
            store,
            lock: &lock,
            trust: &trust,
            default_rate: self.default_rate,
        };
        let mut plan = plan(&bundle, &cx)?;
        if self.require_signed && matches!(bundle.signed, Signed::No) {
            plan.warnings.insert(
                0,
                "[bundles] require_signed is on, so this unsigned bundle can't be installed".into(),
            );
        }
        Ok(Prepared { bundle, plan })
    }

    /// Install (or update to) a prepared bundle. The user has approved
    /// `p.plan`.
    pub fn install(
        &self,
        p: &Prepared,
        store: &Store,
        opts: &InstallOptions,
    ) -> Result<Outcome, BundleError> {
        let b = &p.bundle;
        let name = b.manifest.bundle.name.clone();
        if self.require_signed && matches!(b.signed, Signed::No) {
            return Err(BundleError::Refused(
                "[bundles] require_signed is on and this bundle is unsigned".into(),
            ));
        }
        let conflicts = p.plan.conflicts();
        if !conflicts.is_empty() {
            return Err(BundleError::Refused(conflicts.join("; ")));
        }
        let mut lock = Lock::load(&self.paths.lock())?;
        let previous = lock.get(&name).cloned();
        let key = match &b.signed {
            Signed::By(k) => Some(k.to_base64()),
            Signed::No => None,
        };
        if let Some(prev) = &previous
            && prev.key.is_some()
            && prev.key != key
            && !opts.accept_new_key
        {
            return Err(BundleError::Refused(format!(
                "the installed {name} was signed by {}, and this one {}; install it with --accept-new-key if you know why",
                prev.signature_words(),
                match &key {
                    Some(_) => "by a different key",
                    None => "isn't signed",
                }
            )));
        }
        let agent = resolve_agent(
            store,
            opts.agent
                .as_deref()
                .or(previous.as_ref().and_then(|p| p.agent.as_deref())),
        )?;
        let mut out = Outcome {
            name: name.clone(),
            lines: Vec::new(),
        };

        // Connectors, as drafts.
        let drafts = self.home.drafts_dir();
        std::fs::create_dir_all(&drafts).map_err(io)?;
        let mut connectors = Vec::new();
        let mut skills: BTreeMap<String, Vec<(&str, &[u8])>> = BTreeMap::new();
        let mut agent_slugs: Vec<String> = Vec::new();
        for (path, bytes) in &b.files {
            match kind(path)? {
                Kind::Connector(c) => {
                    std::fs::write(drafts.join(format!("{c}.luau")), bytes).map_err(io)?;
                    let active = self
                        .home
                        .connectors_dir()
                        .join(format!("{c}.luau"))
                        .exists();
                    out.lines.push(if active {
                        format!("connector {c}: new version saved as a draft; activate it to replace the active one")
                    } else {
                        format!("connector {c}: saved as a draft; activate it to use it")
                    });
                    connectors.push(c);
                }
                Kind::Skill(slug) => skills.entry(slug).or_default().push((path, bytes)),
                Kind::Agent(slug) => {
                    if !agent_slugs.contains(&slug) {
                        agent_slugs.push(slug);
                    }
                }
                Kind::Routines | Kind::Readme => {}
            }
        }
        // Skills, replacing this bundle's earlier copy.
        let skills_dir = self.home.skills_dir();
        for (slug, files) in &skills {
            let dir = skills_dir.join(slug);
            if dir.exists() {
                std::fs::remove_dir_all(&dir).map_err(io)?;
            }
            for (path, bytes) in files {
                let rel = path.trim_start_matches(&format!("skills/{slug}/"));
                let target = dir.join(rel);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(io)?;
                }
                std::fs::write(&target, bytes).map_err(io)?;
            }
            out.lines.push(format!("skill {slug}: installed"));
        }
        // Agents: made from the templates, never over one you already have.
        let mut agents: Vec<InstalledAgent> = Vec::new();
        for slug in &agent_slugs {
            let made = previous
                .as_ref()
                .and_then(|p| p.agents.iter().find(|a| a.slug == *slug));
            let existing = store
                .agent(slug)
                .map_err(|e| BundleError::Io(e.to_string()))?;
            match (existing, made) {
                (Some(a), Some(m)) if a.id == m.id => {
                    out.lines.push(format!(
                        "agent @{slug}: already installed; left as you have it"
                    ));
                    agents.push(m.clone());
                }
                (Some(_), _) => {
                    return Err(BundleError::Refused(format!(
                        "you already have an agent @{slug}; rename or archive it first"
                    )));
                }
                (None, _) => {
                    let id = self.make_agent(b, slug, store)?;
                    out.lines.push(format!(
                        "agent @{slug}: created, with its own DM; set its model and anything else in the app"
                    ));
                    agents.push(InstalledAgent {
                        slug: slug.clone(),
                        id,
                    });
                }
            }
        }
        // Routines: unchanged ones are kept (on or off as the user left
        // them); new or changed ones are created paused.
        let templates = match b.file_text(ROUTINES) {
            Some(t) => RoutinesFile::parse(&t)?.routines,
            None => Vec::new(),
        };
        let old_templates = self.installed_templates(&name);
        let mut routines = Vec::new();
        for t in &templates {
            let kept = previous.as_ref().and_then(|prev| {
                let id = &prev.routines.iter().find(|r| r.name == t.name)?.id;
                let same = old_templates.iter().any(|o| o == t);
                (same && store.schedule(id).ok().flatten().is_some()).then(|| id.clone())
            });
            if let Some(id) = kept {
                routines.push(InstalledRoutine {
                    name: t.name.clone(),
                    id,
                });
                continue;
            }
            if let Some(old) = previous
                .as_ref()
                .and_then(|prev| prev.routines.iter().find(|r| r.name == t.name))
            {
                let _ = store.delete_schedule(&old.id);
            }
            // Its own agent, else the bundle's only one, else the one it
            // is installed for.
            let owner = match (&t.agent, agents.as_slice()) {
                (Some(s), _) => agents.iter().find(|a| a.slug == *s).map(|a| a.id.clone()),
                (None, [only]) => Some(only.id.clone()),
                _ => None,
            }
            .unwrap_or_else(|| agent.clone());
            let id = create_routine(store, t, &owner)?;
            out.lines.push(format!(
                "routine {}: created paused ({}); turn it on to start it",
                t.name,
                t.trigger.describe()
            ));
            routines.push(InstalledRoutine {
                name: t.name.clone(),
                id,
            });
        }
        // What the previous version had and this one doesn't.
        if let Some(prev) = &previous {
            for c in prev.connectors.iter().filter(|c| !connectors.contains(c)) {
                out.lines.extend(self.remove_connector(&name, c));
            }
            for s in prev.skills.iter().filter(|s| !skills.contains_key(*s)) {
                let _ = std::fs::remove_dir_all(skills_dir.join(s));
                out.lines
                    .push(format!("skill {s}: removed (no longer in the bundle)"));
            }
            for a in prev
                .agents
                .iter()
                .filter(|a| !agents.iter().any(|n| n.slug == a.slug))
            {
                out.lines.push(format!(
                    "agent @{}: no longer in the bundle; it stays, and is yours now",
                    a.slug
                ));
            }
            for r in prev
                .routines
                .iter()
                .filter(|r| !routines.iter().any(|n| n.name == r.name))
            {
                let _ = store.delete_schedule(&r.id);
                out.lines.push(format!(
                    "routine {}: removed (no longer in the bundle)",
                    r.name
                ));
            }
        }

        self.keep_copy(b)?;
        lock.put(InstalledBundle {
            name: name.clone(),
            version: b.manifest.bundle.version.clone(),
            source: b.origin.to_string(),
            sha256: b.sha256.clone(),
            key,
            agent: (agent != tern_store::DEFAULT_AGENT).then_some(agent),
            installed_at: now(),
            connectors,
            skills: skills.into_keys().collect(),
            routines,
            agents,
            key_trusted: false,
        });
        lock.save(&self.paths.lock())?;
        tracing::info!(bundle = %name, version = %b.manifest.bundle.version, sha256 = %b.sha256, source = %b.origin, "bundle installed");
        Ok(out)
    }

    /// Remove an installed bundle and what it installed. A connector the
    /// user has changed since is left in place.
    pub fn remove(&self, name: &str, store: &Store) -> Result<Outcome, BundleError> {
        let mut lock = Lock::load(&self.paths.lock())?;
        let b = lock
            .remove(name)
            .ok_or_else(|| BundleError::Invalid(format!("no bundle named {name} is installed")))?;
        let mut out = Outcome {
            name: name.to_owned(),
            lines: Vec::new(),
        };
        for c in &b.connectors {
            out.lines.extend(self.remove_connector(name, c));
        }
        for s in &b.skills {
            let _ = std::fs::remove_dir_all(self.home.skills_dir().join(s));
            out.lines.push(format!("skill {s}: removed"));
        }
        for r in &b.routines {
            let _ = store.delete_schedule(&r.id);
            out.lines.push(format!("routine {}: removed", r.name));
        }
        for a in &b.agents {
            // Archived, never deleted: its history and persona stay. One
            // the user deleted is already gone.
            let live = store.agent(&a.id).ok().flatten().is_some();
            let archived = live
                && store
                    .update_agent(
                        &a.id,
                        &tern_store::AgentChange {
                            archived: Some(true),
                            ..tern_store::AgentChange::default()
                        },
                    )
                    .unwrap_or(false);
            out.lines.push(if archived {
                format!(
                    "agent @{}: archived (its persona and history are kept)",
                    a.slug
                )
            } else {
                format!("agent @{}: already gone", a.slug)
            });
        }
        let _ = std::fs::remove_dir_all(self.paths.copy(name));
        lock.save(&self.paths.lock())?;
        tracing::info!(bundle = %name, "bundle removed");
        Ok(out)
    }

    pub fn list(&self) -> Vec<InstalledBundle> {
        crate::installed(&self.home)
    }

    pub fn installed(&self, name: &str) -> Result<Option<InstalledBundle>, BundleError> {
        Ok(Lock::load(&self.paths.lock())?.get(name).cloned())
    }

    /// Make the agent `slug` from the bundle's template: its persona file,
    /// its row and its DM. Returns its id.
    fn make_agent(&self, b: &Bundle, slug: &str, store: &Store) -> Result<String, BundleError> {
        let text = |part: &str| {
            b.file_text(&format!("agents/{slug}/{part}"))
                .ok_or_else(|| {
                    BundleError::Invalid(format!("agent {slug} needs agents/{slug}/{part}"))
                })
        };
        let t = AgentTemplate::parse(slug, &text(AGENT_FILE)?)?;
        let persona = text(PERSONA_FILE)?;
        let invalid = |e: String| BundleError::Invalid(format!("agent @{slug}: {e}"));
        tern_core::agents::check_slug(slug).map_err(invalid)?;
        let mut id = tern_core::agents::id_for(slug);
        match store
            .agent_or_deleted(&id)
            .map_err(|e| BundleError::Io(e.to_string()))?
        {
            None => {}
            Some(old) if old.deleted_at.is_none() => {
                return Err(invalid(format!("the id {id} is taken")));
            }
            // A deleted agent keeps its id (history names it): this one
            // gets the next free id.
            Some(_) => {
                let mut n = 2;
                while store
                    .agent_or_deleted(&tern_core::agents::id_for(&format!("{slug}-{n}")))
                    .map_err(|e| BundleError::Io(e.to_string()))?
                    .is_some()
                {
                    n += 1;
                }
                id = tern_core::agents::id_for(&format!("{slug}-{n}"));
            }
        }
        if store
            .channel(&tern_store::dm_slug(slug))
            .map_err(|e| BundleError::Io(e.to_string()))?
            .is_some()
        {
            return Err(invalid(format!(
                "the channel #{} is taken",
                tern_store::dm_slug(slug)
            )));
        }
        let persona_path = tern_core::agents::persona_path(&self.home, &id);
        if let Some(dir) = persona_path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = persona_path.with_extension("md.tmp");
        std::fs::write(&tmp, format!("{}\n", persona.trim())).map_err(io)?;
        std::fs::rename(&tmp, &persona_path).map_err(io)?;
        let row = tern_store::AgentRow {
            id: id.clone(),
            slug: slug.to_owned(),
            name: t.name.trim().to_owned(),
            avatar: t
                .avatar
                .as_ref()
                .map(|a| serde_json::json!({ "glyph": a.glyph, "color": a.color }).to_string()),
            tools: Some(
                serde_json::to_string(&t.scope()).map_err(|e| BundleError::Io(e.to_string()))?,
            ),
            memory: t.memory.clone().unwrap_or_else(|| "private".into()),
            autonomy: t.autonomy.clone(),
            ..tern_store::AgentRow::default()
        };
        if let Err(e) = store.create_agent(&row) {
            let _ = std::fs::remove_file(&persona_path);
            return Err(BundleError::Io(e.to_string()));
        }
        Ok(id)
    }

    /// Remove connector `c` as bundle `bundle` installed it: its draft, and
    /// the active file while it is still exactly the bundle's.
    fn remove_connector(&self, bundle: &str, c: &str) -> Vec<String> {
        let ours = std::fs::read(self.paths.copy(bundle).join(format!("connectors/{c}.luau"))).ok();
        let mut lines = Vec::new();
        for (where_, path) in [
            ("draft", self.home.drafts_dir().join(format!("{c}.luau"))),
            (
                "active",
                self.home.connectors_dir().join(format!("{c}.luau")),
            ),
        ] {
            let Ok(now) = std::fs::read(&path) else {
                continue;
            };
            if ours.as_deref() == Some(now.as_slice()) {
                let _ = std::fs::remove_file(&path);
                lines.push(format!("connector {c}: {where_} removed"));
            } else {
                lines.push(format!(
                    "connector {c}: the {where_} one was changed after installing, so it was kept"
                ));
            }
        }
        lines
    }

    fn installed_templates(&self, name: &str) -> Vec<RoutineTemplate> {
        std::fs::read_to_string(self.paths.copy(name).join(ROUTINES))
            .ok()
            .and_then(|t| RoutinesFile::parse(&t).ok())
            .map(|f| f.routines)
            .unwrap_or_default()
    }

    /// Keep the approved bundle's files, to diff the next update against
    /// and to know what `remove` may delete.
    fn keep_copy(&self, b: &Bundle) -> Result<(), BundleError> {
        let dir = self.paths.copy(&b.manifest.bundle.name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(io)?;
        }
        let mut files: BTreeMap<String, &[u8]> = b
            .files
            .iter()
            .map(|(k, v)| (k.clone(), v.as_slice()))
            .collect();
        files.insert(crate::manifest::MANIFEST.into(), b.manifest_text.as_bytes());
        if let Some(sig) = &b.signature {
            files.insert(crate::manifest::SIGNATURE.into(), sig.as_bytes());
        }
        for (path, bytes) in files {
            let target = dir.join(&path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            std::fs::write(target, bytes).map_err(io)?;
        }
        Ok(())
    }
}

fn io(e: std::io::Error) -> BundleError {
    BundleError::Io(e.to_string())
}

fn resolve_agent(store: &Store, want: Option<&str>) -> Result<String, BundleError> {
    let Some(want) = want.map(str::trim).filter(|w| !w.is_empty()) else {
        return Ok(tern_store::DEFAULT_AGENT.into());
    };
    store
        .agent(want.trim_start_matches('@'))
        .map_err(|e| BundleError::Io(e.to_string()))?
        .filter(|a| a.archived_at.is_none())
        .map(|a| a.id)
        .ok_or_else(|| BundleError::Invalid(format!("no agent {want:?}")))
}

/// Create a routine from a template, paused, owned by `agent`.
fn create_routine(store: &Store, t: &RoutineTemplate, agent: &str) -> Result<String, BundleError> {
    let bad = |e: String| BundleError::Invalid(format!("routine {}: {e}", t.name));
    let trigger = TriggerArgs {
        cron: t.trigger.cron.clone(),
        at: t.trigger.at.clone(),
        watch: t.trigger.watch.clone(),
        every: t.trigger.every.clone(),
        ..TriggerArgs::default()
    };
    let budget = t.budget.clone().unwrap_or_default();
    let limits = ScheduleLimits {
        quiet_hours: t.quiet_hours.clone().map(Some),
        urgent: t.urgent,
        runs_per_day: budget.runs_per_day.map(Some),
        usd_per_day: budget.usd_per_day.map(Some),
        tokens_per_run: budget.tokens_per_run.map(Some),
        gate: t.gate.clone().map(Some),
        gate_connector: t.gate_connector.clone().map(Some),
        initiative: t.initiative.clone().map(Some),
    };
    let row =
        tern_tools::schedule::create_routine(store, &trigger, &t.prompt, None, &limits, agent)
            .map_err(bad)?;
    let mut row = store
        .schedule(&row.id)
        .map_err(|e| bad(e.to_string()))?
        .ok_or_else(|| bad("vanished after it was made".into()))?;
    row.enabled = false;
    store
        .update_schedule(&row)
        .map_err(|e| bad(e.to_string()))?;
    if let Some(a) = &t.autonomy {
        store
            .set_schedule_autonomy(&row.id, Some(a))
            .map_err(|e| bad(e.to_string()))?;
    }
    Ok(row.id)
}
