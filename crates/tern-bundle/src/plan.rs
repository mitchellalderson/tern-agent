//! What installing a bundle would do, in full, for the user to approve
//! (tooling spec §6.5): every connector's hosts and paths, credentials,
//! OAuth scopes, rate and tool effects; every skill and the files in it
//! that could be run; every routine and its trigger; the secrets and
//! accounts it needs; and anything suspicious. On an update, also what
//! changed since the approved version, and whether it grew.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use tern_core::config::Rate;
use tern_core::{Secrets, TernHome};
use tern_lua::{Host, Vm};
use tern_store::Store;

use crate::manifest::{Kind, ROUTINES, RoutineTemplate, RoutinesFile, kind};
use crate::source::{Bundle, Signed};
use crate::state::{InstalledBundle, Lock, TrustStore};
use crate::{BundleError, lookalike};

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub source: String,
    pub sha256: String,
    pub signature: SignatureInfo,
    pub connectors: Vec<ConnectorPlan>,
    pub skills: Vec<SkillPlan>,
    pub routines: Vec<RoutinePlan>,
    /// Secrets the connectors use: set them before activating.
    pub secrets: Vec<SecretNeed>,
    /// Accounts to sign in to: bundles carry the requirement, never tokens.
    pub connections: Vec<ConnectionNeed>,
    /// Everything the user must not miss, also repeated where it applies.
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readme: Option<String>,
    /// The installed version, when this is an update.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed: Option<InstalledVersion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<Changes>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignatureInfo {
    /// `unsigned`, `trusted` (a key in the trust store), `pinned` (the key
    /// the installed version was signed with), `unknown` (valid, but by a
    /// key the user hasn't trusted) or `new_key` (an update signed by a
    /// different key than the installed one).
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    /// The name it has in the trust store.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trusted_as: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectorPlan {
    pub name: String,
    pub description: String,
    /// `http` allowlist entries: hosts, optionally with paths.
    pub hosts: Vec<String>,
    /// How tern sends credentials (the script never sees them).
    pub auth: Vec<String>,
    /// The secret names `auth` sends.
    pub auth_secrets: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<String>,
    /// Secrets the script reads itself (legacy `ctx.secret`).
    pub raw_secrets: Vec<String>,
    pub rate: String,
    pub tools: Vec<ToolPlan>,
    pub warnings: Vec<String>,
    /// `active` or `draft` when one of that name exists already.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolPlan {
    pub name: String,
    pub description: String,
    /// `read`, `write` or `destructive`.
    pub effect: String,
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillPlan {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub files: Vec<String>,
    /// Files the agent could run (scripts), not just read.
    pub runnable: Vec<String>,
    pub existing: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoutinePlan {
    pub name: String,
    pub trigger: String,
    pub prompt: String,
    pub initiative: String,
    pub autonomy: String,
    pub limits: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretNeed {
    pub name: String,
    pub used_by: Vec<String>,
    pub set: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionNeed {
    pub connector: String,
    pub provider: String,
    pub scopes: Vec<String>,
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledVersion {
    pub version: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Changes {
    /// Capabilities the new version has and the installed one didn't.
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Same capability, different content (a prompt, a skill's text).
    pub changed: Vec<String>,
}

impl Changes {
    /// Whether the update can do something the approved version couldn't:
    /// then it must be approved again.
    pub fn grows(&self) -> bool {
        !self.added.is_empty()
    }
}

/// What a plan is made against.
pub struct Context<'a> {
    pub home: &'a TernHome,
    pub host: &'a Host,
    pub secrets: &'a Secrets,
    pub store: Option<&'a Store>,
    pub lock: &'a Lock,
    pub trust: &'a TrustStore,
    pub default_rate: Rate,
}

/// A connector host for looking at bundled connectors: real secrets (to
/// say which are set) but a scratch store, so nothing is written.
pub fn scratch_host(secrets: Secrets, default_rate: Rate) -> Result<Host, BundleError> {
    let store = Store::open_in_memory().map_err(|e| BundleError::Io(e.to_string()))?;
    let mut host = Host::new(store, secrets).map_err(|e| BundleError::Io(e.to_string()))?;
    host.default_rate = default_rate;
    Ok(host)
}

pub fn plan(bundle: &Bundle, cx: &Context) -> Result<Plan, BundleError> {
    let m = &bundle.manifest.bundle;
    let installed = cx.lock.get(&m.name);
    let mut warnings = Vec::new();

    let signature = signature_info(bundle, installed, cx.trust);
    match signature.state.as_str() {
        "unsigned" => warnings.push(
            "unsigned: nothing shows who made this bundle or that it wasn't changed on the way".into(),
        ),
        "unknown" => warnings.push(format!(
            "signed by key {}, which you haven't trusted: check it is the author's (tern bundle trust adds it)",
            signature.key_id.as_deref().unwrap_or("?")
        )),
        "new_key" => warnings.push(format!(
            "signed by a different key ({}) than the installed version: the author changed keys, or someone else made this",
            signature.key_id.as_deref().unwrap_or("?")
        )),
        _ => {}
    }
    if let Some(i) = installed
        && i.key.is_some()
        && matches!(bundle.signed, Signed::No)
    {
        warnings.push("the installed version was signed and this one isn't".into());
    }

    let mut connectors = Vec::new();
    let mut skills: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut routines_text = None;
    let mut readme = None;
    for (path, bytes) in &bundle.files {
        match kind(path)? {
            Kind::Connector(name) => {
                let text = String::from_utf8(bytes.clone())
                    .map_err(|_| BundleError::Invalid(format!("{path} isn't UTF-8")))?;
                connectors.push(connector_plan(&name, path, &text, cx, &m.name)?);
            }
            Kind::Skill(slug) => skills.entry(slug).or_default().push(path.clone()),
            Kind::Routines => routines_text = Some(String::from_utf8_lossy(bytes).into_owned()),
            Kind::Readme => readme = Some(String::from_utf8_lossy(bytes).into_owned()),
        }
    }
    for c in &connectors {
        for w in &c.warnings {
            warnings.push(format!("connector {}: {w}", c.name));
        }
    }

    let mut skill_plans = Vec::new();
    for (slug, files) in skills {
        let p = skill_plan(bundle, &slug, files, cx, &m.name)?;
        if !p.runnable.is_empty() {
            warnings.push(format!(
                "skill {} includes files the agent could run: {}",
                p.slug,
                p.runnable.join(", ")
            ));
        }
        skill_plans.push(p);
    }

    let templates = match &routines_text {
        Some(t) => RoutinesFile::parse(t)?.routines,
        None => Vec::new(),
    };
    let bundled: BTreeSet<&str> = connectors.iter().map(|c| c.name.as_str()).collect();
    let routines = templates
        .iter()
        .map(|r| routine_plan(r, &bundled, cx))
        .collect::<Result<Vec<_>, _>>()?;

    let (secrets, connections) = needs(&connectors, cx);

    let mut plan = Plan {
        name: m.name.clone(),
        version: m.version.clone(),
        description: m.description.clone(),
        author: m.author.clone(),
        source: bundle.origin.to_string(),
        sha256: bundle.sha256.clone(),
        signature,
        connectors,
        skills: skill_plans,
        routines,
        secrets,
        connections,
        warnings,
        readme: readme.map(|r| r.chars().take(4000).collect()),
        installed: installed.map(|i| InstalledVersion {
            version: i.version.clone(),
            sha256: i.sha256.clone(),
        }),
        changes: None,
    };
    if let Some(old) = previous_plan(cx, &m.name)? {
        plan.changes = Some(diff(&old, &plan));
    }
    Ok(plan)
}

fn signature_info(
    bundle: &Bundle,
    installed: Option<&InstalledBundle>,
    trust: &TrustStore,
) -> SignatureInfo {
    let Signed::By(key) = &bundle.signed else {
        return SignatureInfo {
            state: "unsigned".into(),
            key: None,
            key_id: None,
            trusted_as: None,
        };
    };
    let trusted = trust.find(key);
    let pinned = installed.and_then(|i| i.key.as_deref());
    let same_as_pinned =
        pinned.is_some_and(|p| crate::PublicKey::parse(p).is_ok_and(|p| p == *key));
    let state = if pinned.is_some() && !same_as_pinned {
        "new_key"
    } else if trusted.is_some() {
        "trusted"
    } else if same_as_pinned {
        "pinned"
    } else {
        "unknown"
    };
    SignatureInfo {
        state: state.into(),
        key: Some(key.to_base64()),
        key_id: Some(key.id_hex()),
        trusted_as: trusted.map(|t| t.name.clone()).filter(|n| !n.is_empty()),
    }
}

fn connector_plan(
    name: &str,
    path: &str,
    source: &str,
    cx: &Context,
    bundle: &str,
) -> Result<ConnectorPlan, BundleError> {
    let file = path.rsplit('/').next().unwrap_or(path);
    let vm = Vm::load(cx.host, file, source)
        .map_err(|e| BundleError::Invalid(format!("{path}: {e}")))?;
    let s = &vm.spec;
    if s.name != name {
        return Err(BundleError::Invalid(format!(
            "{path} declares name {:?}; it must match the file name",
            s.name
        )));
    }
    let c = &s.capabilities;
    let mut warnings = lookalike::check(name, &c.http)
        .into_iter()
        .map(|w| format!("lookalike host: {w}"))
        .collect::<Vec<_>>();
    if c.legacy_secrets() {
        warnings.push(format!(
            "reads {} itself (legacy ctx.secret), so it could send the value to any host it may reach",
            c.secrets.join(", ")
        ));
    }
    let tools = s
        .tools
        .iter()
        .map(|t| ToolPlan {
            name: t.name.clone(),
            description: t.description.clone(),
            effect: t.effect().to_string(),
            flags: [
                (t.untrusted, "returns third-party text"),
                (t.read_via_post, "reads via POST"),
                (t.requires_approval, "always asks"),
                (t.forbidden.is_some(), "refused: moves money or trades"),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, f)| (*f).to_owned())
            .collect(),
        })
        .collect();
    let active = cx.home.connectors_dir().join(format!("{name}.luau"));
    let draft = cx.home.drafts_dir().join(format!("{name}.luau"));
    let owner = cx.lock.owner_of_connector(name).map(|b| b.name.as_str());
    let existing = if active.exists() {
        Some("active")
    } else if draft.exists() {
        Some("draft")
    } else {
        None
    };
    if existing.is_some() && owner != Some(bundle) {
        warnings.push(match owner {
            Some(o) => format!("bundle {o} already installed a connector named {name}"),
            None => format!(
                "you already have a connector named {name} ({}); remove or rename it first",
                existing.unwrap_or_default()
            ),
        });
    }
    Ok(ConnectorPlan {
        name: name.to_owned(),
        description: s.description.clone(),
        hosts: c.http.clone(),
        // OAuth entries are the `oauth` line's sign-in, not a secret sent.
        auth: c
            .auth
            .iter()
            .filter(|a| !a.secrets().iter().all(|s| s.starts_with("oauth:")))
            .map(tern_lua::Auth::describe)
            .collect(),
        auth_secrets: c
            .auth_secrets()
            .into_iter()
            .filter(|s| !s.starts_with("oauth:"))
            .collect(),
        oauth: c.oauth.as_ref().map(|o| o.describe()),
        raw_secrets: c.secrets.clone(),
        rate: match c.rate {
            Some(r) => r.to_string(),
            None => format!("{} (default)", cx.default_rate),
        },
        tools,
        warnings,
        existing: existing.map(str::to_owned),
    })
}

/// Files in a skill that look like something to run rather than read.
fn runnable(path: &str, bytes: &[u8]) -> bool {
    const EXT: &[&str] = &[
        ".sh", ".bash", ".zsh", ".py", ".js", ".mjs", ".ts", ".rb", ".pl", ".ps1", ".bat", ".cmd",
        ".exe", ".bin", ".lua", ".luau", ".php",
    ];
    let lower = path.to_ascii_lowercase();
    EXT.iter().any(|e| lower.ends_with(e))
        || bytes.starts_with(b"#!")
        || bytes.starts_with(b"\x7fELF")
        || bytes.starts_with(b"\xcf\xfa\xed\xfe")
}

fn skill_plan(
    bundle: &Bundle,
    slug: &str,
    files: Vec<String>,
    cx: &Context,
    bundle_name: &str,
) -> Result<SkillPlan, BundleError> {
    let md_path = format!("skills/{slug}/SKILL.md");
    let text = bundle
        .file_text(&md_path)
        .ok_or_else(|| BundleError::Invalid(format!("skill {slug} has no {md_path}")))?;
    let (name, description, _) = tern_core::skills::parse(&text).ok_or_else(|| {
        BundleError::Invalid(format!(
            "{md_path} needs `name` and `description` frontmatter"
        ))
    })?;
    let runnable: Vec<String> = files
        .iter()
        .filter(|p| bundle.files.get(*p).is_some_and(|b| runnable(p, b)))
        .map(|p| p.trim_start_matches(&format!("skills/{slug}/")).to_owned())
        .collect();
    let dir = cx.home.skills_dir().join(slug);
    let owner = cx.lock.owner_of_skill(slug).map(|b| b.name.as_str());
    Ok(SkillPlan {
        slug: slug.to_owned(),
        name,
        description,
        files: files
            .iter()
            .map(|p| p.trim_start_matches(&format!("skills/{slug}/")).to_owned())
            .collect(),
        runnable,
        existing: dir.exists() && owner != Some(bundle_name),
    })
}

fn routine_plan(
    r: &RoutineTemplate,
    bundled: &BTreeSet<&str>,
    cx: &Context,
) -> Result<RoutinePlan, BundleError> {
    let bad = |why: String| BundleError::Invalid(format!("{ROUTINES}: routine {}: {why}", r.name));
    let t = &r.trigger;
    let mut notes = Vec::new();
    if let Some(c) = &t.cron {
        c.parse::<tern_core::cron::Cron>()
            .map_err(|e| bad(e.to_string()))?;
    }
    if let Some(a) = &t.at {
        let at = tern_tools::schedule::parse_at(a).map_err(bad)?;
        if at <= chrono::Local::now() {
            return Err(bad(format!("{a} has already passed")));
        }
    }
    if let Some(w) = &t.watch {
        let (connector, _) = tern_tools::schedule::split_watch(w).map_err(bad)?;
        if !bundled.contains(connector)
            && !cx
                .home
                .connectors_dir()
                .join(format!("{connector}.luau"))
                .exists()
        {
            notes.push(format!(
                "watches connector {connector}, which neither this bundle nor you have"
            ));
        }
        if let Some(e) = &t.every {
            tern_tools::schedule::parse_every(e).map_err(bad)?;
        }
    }
    let initiative = match &r.initiative {
        None => "default (digest)".to_owned(),
        Some(i) => tern_core::initiative::Initiative::parse(i)
            .ok_or_else(|| {
                bad(format!(
                    "initiative {i:?} must be silent, digest, notify or interrupt"
                ))
            })?
            .as_str()
            .to_owned(),
    };
    let autonomy = match &r.autonomy {
        None => "your configured autonomy".to_owned(),
        Some(a) => tern_core::approval::Autonomy::parse(a)
            .ok_or_else(|| bad(format!("autonomy {a:?} isn't one tern knows")))?
            .as_str()
            .to_owned(),
    };
    let mut limits = Vec::new();
    if let Some(q) = &r.quiet_hours {
        q.parse::<tern_core::quiet::QuietHours>()
            .map_err(|e| bad(e.to_string()))?;
        limits.push(format!("quiet hours {q}"));
    }
    if r.urgent == Some(true) {
        limits.push("urgent: delivers inside quiet hours".into());
    }
    if let Some(b) = &r.budget {
        if let Some(n) = b.runs_per_day {
            limits.push(format!("{n} runs a day"));
        }
        if let Some(u) = b.usd_per_day {
            limits.push(format!("${u:.2} a day"));
        }
        if let Some(t) = b.tokens_per_run {
            limits.push(format!("{t} tokens a run"));
        }
    }
    if let Some(g) = &r.gate {
        limits.push(match &r.gate_connector {
            Some(c) => format!("gate (with connector {c}): {g}"),
            None => format!("gate: {g}"),
        });
    }
    if let Some(c) = &r.channel {
        notes.push(format!(
            "asks for channel {c}; results go to the agent's usual place (move it after installing)"
        ));
    }
    if let Some(m) = &r.model {
        notes.push(format!(
            "asks for model {m}; per-routine models aren't built, so the agent's model runs it"
        ));
    }
    Ok(RoutinePlan {
        name: r.name.clone(),
        trigger: t.describe(),
        prompt: r.prompt.trim().to_owned(),
        initiative,
        autonomy,
        limits,
        notes,
    })
}

fn needs(connectors: &[ConnectorPlan], cx: &Context) -> (Vec<SecretNeed>, Vec<ConnectionNeed>) {
    let mut secrets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut connections = Vec::new();
    for c in connectors {
        for s in c.auth_secrets.iter().chain(&c.raw_secrets) {
            secrets.entry(s.clone()).or_default().push(c.name.clone());
        }
        if let Some(o) = &c.oauth {
            let (provider, scopes) = match o.split_once(" (scopes: ") {
                Some((p, s)) => (
                    p.to_owned(),
                    s.trim_end_matches(')')
                        .split(", ")
                        .map(str::to_owned)
                        .collect(),
                ),
                None => (o.clone(), Vec::new()),
            };
            let connected = cx
                .store
                .and_then(|s| s.connections("lua", &c.name).ok())
                .is_some_and(|v| !v.is_empty());
            connections.push(ConnectionNeed {
                connector: c.name.clone(),
                provider,
                scopes,
                connected,
            });
        }
    }
    let secrets = secrets
        .into_iter()
        .map(|(name, mut used_by)| {
            used_by.dedup();
            SecretNeed {
                set: cx.secrets.get(&name).is_some(),
                name,
                used_by,
            }
        })
        .collect();
    (secrets, connections)
}

/// The plan of the installed copy, to compare an update with.
fn previous_plan(cx: &Context, name: &str) -> Result<Option<Plan>, BundleError> {
    let Some(installed) = cx.lock.get(name) else {
        return Ok(None);
    };
    let dir = crate::Paths::new(cx.home).copy(name);
    if !dir.exists() {
        return Ok(None);
    }
    let raw = crate::source::read_path(&dir)?;
    let old = crate::source::verify(raw, None)?;
    // Compared without the lockfile, so the old plan doesn't diff itself.
    let empty = Lock::default();
    let cx_old = Context {
        lock: &empty,
        ..*cx
    };
    let mut p = plan(&old, &cx_old)?;
    p.sha256 = installed.sha256.clone();
    Ok(Some(p))
}

/// One line per capability, so two versions can be compared.
pub fn capability_lines(p: &Plan) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for c in &p.connectors {
        let n = &c.name;
        out.insert(format!("connector {n}"));
        for h in &c.hosts {
            out.insert(format!("connector {n}: reaches {h}"));
        }
        for a in &c.auth {
            out.insert(format!("connector {n}: auth {a}"));
        }
        if let Some(o) = &c.oauth {
            out.insert(format!("connector {n}: oauth {o}"));
        }
        for s in &c.raw_secrets {
            out.insert(format!("connector {n}: reads secret {s} itself"));
        }
        out.insert(format!("connector {n}: rate {}", c.rate));
        for t in &c.tools {
            out.insert(format!("connector {n}: tool {} ({})", t.name, t.effect));
        }
    }
    for s in &p.skills {
        out.insert(format!("skill {}", s.slug));
        for r in &s.runnable {
            out.insert(format!("skill {}: runnable {r}", s.slug));
        }
    }
    for r in &p.routines {
        out.insert(format!("routine {}: {}", r.name, r.trigger));
        out.insert(format!("routine {}: initiative {}", r.name, r.initiative));
        out.insert(format!("routine {}: autonomy {}", r.name, r.autonomy));
    }
    out
}

pub fn diff(old: &Plan, new: &Plan) -> Changes {
    let (a, b) = (capability_lines(old), capability_lines(new));
    let mut changed = Vec::new();
    for r in &new.routines {
        if let Some(o) = old.routines.iter().find(|o| o.name == r.name)
            && (o.prompt != r.prompt || o.limits != r.limits)
        {
            changed.push(format!("routine {}: prompt or limits", r.name));
        }
    }
    for s in &new.skills {
        if let Some(o) = old.skills.iter().find(|o| o.slug == s.slug)
            && (o.files != s.files || o.description != s.description)
        {
            changed.push(format!("skill {}: files or description", s.slug));
        }
    }
    Changes {
        added: b.difference(&a).cloned().collect(),
        removed: a.difference(&b).cloned().collect(),
        changed,
    }
}

impl Plan {
    /// Whether it can't be installed as it is (a name taken by something
    /// that isn't this bundle's).
    pub fn conflicts(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .connectors
            .iter()
            .flat_map(|c| c.warnings.iter())
            .filter(|w| w.contains("already"))
            .cloned()
            .collect();
        for s in self.skills.iter().filter(|s| s.existing) {
            out.push(format!(
                "you already have a skill in skills/{}; remove or rename it first",
                s.slug
            ));
        }
        out
    }

    /// The whole plan in words, for the terminal.
    pub fn render(&self) -> String {
        let mut o = String::new();
        let mut line = |s: String| {
            o.push_str(&s);
            o.push('\n');
        };
        let title = if self.description.is_empty() {
            format!("Bundle {} {}", self.name, self.version)
        } else {
            format!(
                "Bundle {} {} — {}",
                self.name, self.version, self.description
            )
        };
        line(title);
        if !self.author.is_empty() {
            line(format!("  by {}", self.author));
        }
        line(format!("  from {}", self.source));
        line(format!("  sha256 {}", self.sha256));
        let sig = &self.signature;
        line(match sig.state.as_str() {
            "unsigned" => "  ⚠ unsigned".into(),
            "trusted" => format!(
                "  ✓ signed by {} (trusted{})",
                sig.key_id.as_deref().unwrap_or("?"),
                sig.trusted_as
                    .as_deref()
                    .map(|n| format!(": {n}"))
                    .unwrap_or_default()
            ),
            "pinned" => format!(
                "  ✓ signed by {} (the key the installed version was signed with)",
                sig.key_id.as_deref().unwrap_or("?")
            ),
            "new_key" => format!(
                "  ⚠ signed by a NEW key {}",
                sig.key_id.as_deref().unwrap_or("?")
            ),
            _ => format!(
                "  ! signed by {}, a key you haven't trusted",
                sig.key_id.as_deref().unwrap_or("?")
            ),
        });
        if let Some(i) = &self.installed {
            line(format!("  replaces installed version {}", i.version));
        }
        if !self.connectors.is_empty() {
            line(String::new());
            line("Connectors (installed inactive: each is activated separately, with its own approval):".into());
            for c in &self.connectors {
                line(format!("  {} — {}", c.name, c.description));
                line(format!(
                    "    reaches: {}",
                    if c.hosts.is_empty() {
                        "nothing (no network)".into()
                    } else {
                        c.hosts.join(", ")
                    }
                ));
                for a in &c.auth {
                    line(format!(
                        "    auth (tern sends it, the script never sees it): {a}"
                    ));
                }
                if let Some(oa) = &c.oauth {
                    line(format!("    OAuth sign-in: {oa}"));
                }
                line(format!("    rate: {}", c.rate));
                for t in &c.tools {
                    let flags = if t.flags.is_empty() {
                        String::new()
                    } else {
                        format!(", {}", t.flags.join(", "))
                    };
                    line(format!(
                        "    tool {} ({}{flags}): {}",
                        t.name, t.effect, t.description
                    ));
                }
                for w in &c.warnings {
                    line(format!("    ⚠ {w}"));
                }
            }
        }
        if !self.skills.is_empty() {
            line(String::new());
            line("Skills (added to what the agent can read):".into());
            for s in &self.skills {
                line(format!(
                    "  {} — {} ({} file{})",
                    s.name,
                    s.description,
                    s.files.len(),
                    if s.files.len() == 1 { "" } else { "s" }
                ));
                if !s.runnable.is_empty() {
                    line(format!("    ⚠ can be run: {}", s.runnable.join(", ")));
                }
            }
        }
        if !self.routines.is_empty() {
            line(String::new());
            line("Routines (installed paused: turn each on to start it):".into());
            for r in &self.routines {
                line(format!(
                    "  {} — {}; initiative {}; autonomy {}",
                    r.name, r.trigger, r.initiative, r.autonomy
                ));
                let prompt: String = r.prompt.chars().take(200).collect();
                line(format!("    prompt: {prompt}"));
                if !r.limits.is_empty() {
                    line(format!("    limits: {}", r.limits.join("; ")));
                }
                for n in &r.notes {
                    line(format!("    note: {n}"));
                }
            }
        }
        if !self.secrets.is_empty() || !self.connections.is_empty() {
            line(String::new());
            line("Needs (nothing secret comes with a bundle):".into());
            for s in &self.secrets {
                line(format!(
                    "  secret {} for {}{}",
                    s.name,
                    s.used_by.join(", "),
                    if s.set {
                        " (set)"
                    } else {
                        " (not set: `tern secrets set` or the desktop's Secrets)"
                    }
                ));
            }
            for c in &self.connections {
                let scopes = if c.scopes.is_empty() {
                    String::new()
                } else {
                    format!(" with scopes {}", c.scopes.join(", "))
                };
                line(format!(
                    "  an account: {}{scopes} for {}{}",
                    c.provider,
                    c.connector,
                    if c.connected {
                        " (connected)".into()
                    } else {
                        format!(" (then `tern connections add {}`)", c.connector)
                    }
                ));
            }
        }
        if let Some(ch) = &self.changes {
            line(String::new());
            if ch.added.is_empty() && ch.removed.is_empty() && ch.changed.is_empty() {
                line("Changes from the installed version: none in capabilities.".into());
            } else {
                line("Changes from the installed version:".into());
                for a in &ch.added {
                    line(format!("  + {a}"));
                }
                for r in &ch.removed {
                    line(format!("  - {r}"));
                }
                for c in &ch.changed {
                    line(format!("  ~ {c}"));
                }
            }
        }
        if !self.warnings.is_empty() {
            line(String::new());
            line("Warnings:".into());
            for w in &self.warnings {
                line(format!("  ⚠ {w}"));
            }
        }
        o
    }
}
