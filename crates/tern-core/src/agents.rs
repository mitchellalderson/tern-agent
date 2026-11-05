//! Agent profiles (docs/agent-profiles-design.md): a configurable agent's
//! tool scope, memory scope, templates, and what a run freezes from it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::home::TernHome;

/// The default agent: today's setup, whose DM is Home.
pub const DEFAULT: &str = "tern";

/// Tools that are part of the system layer: every agent has them, whatever
/// its scope (§1.2).
pub const SYSTEM_TOOLS: &[&str] = &[
    "channel_read",
    "channel_post",
    "channel_suggest",
    "history_search",
    "secret_request",
    "memory_save",
    "memory_search",
    "profile_edit",
    "session_search",
];

/// A new agent's id from its slug at creation.
pub fn id_for(slug: &str) -> String {
    format!("{DEFAULT}:{slug}")
}

/// An agent slug: `[a-z0-9][a-z0-9-]{0,31}`.
pub fn check_slug(slug: &str) -> Result<(), String> {
    crate::channels::check_slug(slug).map_err(|_| {
        format!(
            "agent name {slug:?} must be 1–32 lowercase letters, digits or dashes, not starting with a dash"
        )
    })
}

/// A slug from a display name: "Research Bot" → `research-bot`.
pub fn slug_for(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut out: String = out.trim_end_matches('-').chars().take(32).collect();
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Where an agent's files live: `~/.tern/agents/<id without "tern:">/`.
/// The default agent keeps `~/.tern/persona.md` and `~/.tern/memory.md`.
pub fn dir(home: &TernHome, id: &str) -> PathBuf {
    home.root()
        .join("agents")
        .join(id.strip_prefix("tern:").unwrap_or(id))
}

pub fn persona_path(home: &TernHome, id: &str) -> PathBuf {
    if id == DEFAULT {
        home.persona_path()
    } else {
        dir(home, id).join("persona.md")
    }
}

/// The `memory.md` an agent reads and writes: its own when private.
pub fn memory_path(home: &TernHome, id: &str, private: bool) -> PathBuf {
    if private && id != DEFAULT {
        dir(home, id).join("memory.md")
    } else {
        home.memory_path()
    }
}

/// `"all"`, `"read_only"` (built-in tools only), or a list of names.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Pick {
    #[default]
    All,
    ReadOnly,
    Names(Vec<String>),
}

impl Serialize for Pick {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Pick::All => s.serialize_str("all"),
            Pick::ReadOnly => s.serialize_str("read_only"),
            Pick::Names(n) => n.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Pick {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Word(String),
            Names(Vec<String>),
        }
        match Raw::deserialize(d)? {
            Raw::Word(w) if w == "all" => Ok(Pick::All),
            Raw::Word(w) if w == "read_only" => Ok(Pick::ReadOnly),
            Raw::Word(w) => Err(serde::de::Error::custom(format!(
                "expected \"all\", \"read_only\" or a list of names, not {w:?}"
            ))),
            Raw::Names(n) => Ok(Pick::Names(n)),
        }
    }
}

impl Pick {
    fn is_all(&self) -> bool {
        matches!(self, Pick::All)
    }
}

/// What an agent may use (§1.2). A missing key means `all`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolScope {
    #[serde(skip_serializing_if = "Pick::is_all")]
    pub builtins: Pick,
    #[serde(skip_serializing_if = "Pick::is_all")]
    pub connectors: Pick,
    #[serde(skip_serializing_if = "Pick::is_all")]
    pub mcp: Pick,
    #[serde(skip_serializing_if = "Pick::is_all")]
    pub skills: Pick,
}

impl ToolScope {
    /// A new agent's narrow default: built-in tools except `shell`, no
    /// connectors, no MCP servers, all skills.
    pub fn narrow(builtins: &[String]) -> Self {
        Self {
            builtins: Pick::Names(
                builtins
                    .iter()
                    .filter(|n| n.as_str() != "shell")
                    .cloned()
                    .collect(),
            ),
            connectors: Pick::Names(Vec::new()),
            mcp: Pick::Names(Vec::new()),
            skills: Pick::All,
        }
    }

    pub fn parse(json: &str) -> Result<Self, String> {
        let s: Self = serde_json::from_str(json).map_err(|e| format!("tool scope: {e}"))?;
        if matches!(s.connectors, Pick::ReadOnly)
            || matches!(s.mcp, Pick::ReadOnly)
            || matches!(s.skills, Pick::ReadOnly)
        {
            return Err("tool scope: \"read_only\" is only for builtins".into());
        }
        Ok(s)
    }

    /// The frozen `runs.tool_scope` for this scope: tool names, plus
    /// prefix patterns ending in `*` for connectors (`lua__<name>__*`), MCP
    /// servers (`mcp__<name>__*`) and skills (`skill:<name>`), whose tools
    /// may not be listed until they start. `builtins` are the registry's
    /// own tools; the system layer's are always in.
    pub fn freeze(&self, builtins: &[String]) -> Vec<String> {
        let child_only = crate::subagent::CHILD_ONLY_TOOLS;
        let mut out: Vec<String> = match &self.builtins {
            Pick::All => builtins.to_vec(),
            Pick::ReadOnly => builtins
                .iter()
                .filter(|n| crate::subagent::READ_ONLY_TOOLS.contains(&n.as_str()))
                .cloned()
                .collect(),
            Pick::Names(names) => builtins
                .iter()
                .filter(|n| names.contains(n))
                .cloned()
                .collect(),
        };
        out.extend(
            SYSTEM_TOOLS
                .iter()
                .map(|t| (*t).to_owned())
                .filter(|t| builtins.contains(t)),
        );
        out.retain(|n| !child_only.contains(&n.as_str()));
        let mut pattern = |pick: &Pick, all: &str, one: &dyn Fn(&str) -> String| match pick {
            Pick::All | Pick::ReadOnly => out.push(all.to_owned()),
            Pick::Names(names) => out.extend(names.iter().map(|n| one(n))),
        };
        pattern(&self.connectors, "lua__*", &|n| format!("lua__{n}__*"));
        pattern(&self.mcp, "mcp__*", &|n| format!("mcp__{n}__*"));
        pattern(&self.skills, "skill:*", &|n| format!("skill:{n}"));
        out.sort();
        out.dedup();
        out
    }

    /// Whether the built-in tool `name` is in scope (the system layer's
    /// always are).
    pub fn allows_builtin(&self, name: &str) -> bool {
        SYSTEM_TOOLS.contains(&name)
            || match &self.builtins {
                Pick::All => true,
                Pick::ReadOnly => crate::subagent::READ_ONLY_TOOLS.contains(&name),
                Pick::Names(n) => n.iter().any(|x| x == name),
            }
    }

    /// Whether the skill `name` is in scope.
    pub fn allows_skill(&self, name: &str) -> bool {
        match &self.skills {
            Pick::All | Pick::ReadOnly => true,
            Pick::Names(n) => n.iter().any(|s| s == name),
        }
    }
}

/// A frozen `runs.tool_scope` (`None` = everything) allows skill `name`.
pub fn scope_allows_skill(scope: Option<&[String]>, name: &str) -> bool {
    scope.is_none_or(|s| crate::tool::in_scope(Some(s), &format!("skill:{name}")))
}

/// An agent's own Telegram bot (`agents.telegram`, agent profiles §6.4,
/// AP3): the name of the secret that holds its token (never the token),
/// and a Bot API URL that only tests and self-hosted API servers change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTelegram {
    pub bot_token_secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
}

impl AgentTelegram {
    /// The stored JSON, if it is one.
    pub fn parse(stored: Option<&str>) -> Option<Self> {
        stored.and_then(|t| serde_json::from_str(t).ok())
    }
}

/// The routing call's instructions ("Let agents decide", agent profiles
/// §5.2): one cheap call that only picks who answers.
pub const ROUTER_SYSTEM: &str = "You route a user's message in a group chat to the agents best placed to answer it. \
You are given the agents, one per line as `slug · Name · what it is for`. Pick the one agent whose job fits best; pick \
a second or third only when the message clearly has separate parts for them. If none fits or you are unsure, pick the \
first agent listed (the lead). Answer with JSON only, no other text: {\"agents\": [\"slug\", ...]}.";

/// At most this many agents answer one routed message.
pub const ROUTER_MAX_AGENTS: usize = 3;

/// The routing question: the message and the candidates, the lead first.
/// `agents` are `(slug, name, persona line)`.
pub fn router_prompt(message: &str, agents: &[(String, String, String)]) -> String {
    let message: String = message.chars().take(4000).collect();
    let mut out = String::from("Agents:\n");
    for (slug, name, line) in agents {
        out.push_str(&format!("{slug} · {name} · {line}\n"));
    }
    out.push_str("\nMessage to route:\n");
    out.push_str(&message);
    out
}

/// The slugs the routing call chose, leniently read: the first JSON object
/// in `text`, its `agents` list, `@` and case ignored. Only `valid` slugs
/// count, each once, at most [`ROUTER_MAX_AGENTS`]. `None` when nothing
/// usable was chosen (the caller falls back to the lead).
pub fn parse_router_answer(text: &str, valid: &[String]) -> Option<Vec<String>> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let v: serde_json::Value = serde_json::from_str(text.get(start..=end)?).ok()?;
    let mut out: Vec<String> = Vec::new();
    for a in v.get("agents")?.as_array()? {
        let slug = a.as_str()?.trim().trim_start_matches('@').to_lowercase();
        if valid.contains(&slug) && !out.contains(&slug) {
            out.push(slug);
        }
    }
    out.truncate(ROUTER_MAX_AGENTS);
    (!out.is_empty()).then_some(out)
}

/// A starting point for a new agent (§7.2). It only fills the sheet. The
/// catalog is data: `assets/starters.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub description: String,
    /// What UIs group starters by ("Coding", "Everyday", …).
    pub category: String,
    pub persona: String,
    /// `None` = the narrow default.
    #[serde(default)]
    pub tools: Option<ToolScope>,
    pub memory: String,
    /// The autonomy ceiling a new agent gets from this starter (a cap, never
    /// a grant). The catalog only uses `readonly` and `supervised`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<crate::approval::Autonomy>,
    /// A suggested routine: (cron, prompt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine: Option<(String, String)>,
    pub avatar: (String, String),
}

/// The avatar picker's colours and glyphs (the desktop's `AgentAvatar`).
pub const AVATAR_COLORS: &[&str] = &[
    "amber", "teal", "violet", "pink", "blue", "orange", "purple", "green",
];
pub const AVATAR_GLYPHS: &[&str] = &["●", "■", "▲", "★", "♥", "♦", "♣", "♠", "☾", "✓"];

/// The built-in tools a starter may name (the registry's, minus the system
/// layer, which every agent has, and the sub-agent-only ones).
pub const STARTER_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "search_files",
    "write_file",
    "edit_file",
    "shell",
    "python",
    "http_fetch",
    "schedule_create",
    "schedule_list",
    "schedule_update",
    "schedule_delete",
    "skill_read",
];

/// The starters a brand-new home gets as real agents (once; see
/// `Runtime::seed_starter_agents`).
pub const SEEDED_STARTERS: &[&str] = &[
    "assistant",
    "research",
    "code-reviewer",
    "seo",
    "writer-editor",
    "bug-fixer",
];

/// Built-in tools only coding starters may have: they run commands.
/// (File edits are allowed to any starter whose job is making files, such as
/// a writer's drafts; none of them gets `shell`.)
const POWER_TOOLS: &[&str] = &["shell"];
/// Categories whose starters may have [`POWER_TOOLS`].
const POWER_CATEGORIES: &[&str] = &["Coding"];
/// Built-in tools that change something: a `readonly` ceiling on a starter
/// that has any of them would be pointless (every call would be refused).
const WRITE_TOOLS: &[&str] = &[
    "write_file",
    "edit_file",
    "shell",
    "schedule_create",
    "schedule_update",
    "schedule_delete",
];
/// The most autonomy any starter may ship with.
pub const STARTER_MAX_AUTONOMY: crate::approval::Autonomy = crate::approval::Autonomy::Supervised;

impl Template {
    /// The checks a starter must pass: everything a user-created agent is
    /// checked for (name, slug, memory, tool scope), plus the catalog's own
    /// rules (the header of `assets/starters.toml`).
    pub fn validate(&self) -> Result<(), String> {
        let id = &self.id;
        let bad = |what: &str| Err(format!("starter {id:?}: {what}"));
        check_slug(id).map_err(|_| format!("starter id {id:?} must be a slug"))?;
        if self.name.is_empty() && id != "blank" {
            return bad("no name");
        }
        if !self.name.is_empty() {
            check_slug(&slug_for(&self.name)).map_err(|e| format!("starter {id:?}: {e}"))?;
        }
        if self.name.chars().count() > 40 || self.name != self.name.trim() {
            return bad("name is too long or has stray spaces");
        }
        if self.description.is_empty()
            || self.description.contains('\n')
            || self.description.chars().count() > 100
        {
            return bad("description must be one line of at most 100 characters");
        }
        if self.category.trim().is_empty() {
            return bad("no category");
        }
        if !matches!(self.memory.as_str(), "shared" | "private") {
            return bad("memory must be shared or private");
        }
        if !AVATAR_GLYPHS.contains(&self.avatar.0.as_str())
            || !AVATAR_COLORS.contains(&self.avatar.1.as_str())
        {
            return bad("avatar glyph or colour is not one the picker offers");
        }
        let persona = self.persona.trim();
        if persona.is_empty() || persona.chars().count() > 4000 {
            return bad("persona is empty or over 4000 characters");
        }
        if id != "blank" && !persona.lines().any(|l| l.starts_with("- Your job:")) {
            return bad("persona has no \"- Your job:\" line");
        }
        if !persona.starts_with("You are ") {
            return bad("persona should start with \"You are \"");
        }
        if let Some(a) = self.autonomy {
            if a > STARTER_MAX_AUTONOMY {
                return bad("autonomy may be at most supervised");
            }
            if a == crate::approval::Autonomy::ReadOnly {
                // `None` is the narrow default, which includes file writes.
                let writes = match self.tools.as_ref().map(|t| &t.builtins) {
                    None | Some(Pick::All) => true,
                    Some(Pick::ReadOnly) => false,
                    Some(Pick::Names(n)) => n.iter().any(|n| WRITE_TOOLS.contains(&n.as_str())),
                };
                if writes {
                    return bad("a readonly starter must not have write-capable tools");
                }
            }
        }
        if let Some((cron, prompt)) = &self.routine {
            cron.parse::<crate::cron::Cron>()
                .map_err(|e| format!("starter {id:?}: routine cron: {e}"))?;
            if prompt.trim().is_empty() {
                return bad("routine prompt is empty");
            }
        }
        if let Some(tools) = &self.tools {
            // The same parse a created agent's scope goes through.
            let json = serde_json::to_string(tools).map_err(|e| e.to_string())?;
            ToolScope::parse(&json).map_err(|e| format!("starter {id:?}: {e}"))?;
            match &tools.builtins {
                Pick::All => return bad("built-ins must be listed, not \"all\""),
                Pick::ReadOnly => {}
                Pick::Names(names) => {
                    for n in names {
                        if !STARTER_TOOLS.contains(&n.as_str()) {
                            return bad(&format!("unknown built-in tool {n:?}"));
                        }
                        if POWER_TOOLS.contains(&n.as_str())
                            && !POWER_CATEGORIES.contains(&self.category.as_str())
                        {
                            return bad(&format!("{n} is only for coding and data starters"));
                        }
                    }
                }
            }
            if matches!(tools.mcp, Pick::All) {
                return bad("MCP servers must not be \"all\"");
            }
        }
        Ok(())
    }
}

/// Parse and validate a starters file.
pub fn parse_starters(src: &str) -> Result<Vec<Template>, String> {
    #[derive(Deserialize)]
    struct File {
        starter: Vec<Template>,
    }
    let file: File = toml::from_str(src).map_err(|e| format!("starters.toml: {e}"))?;
    let mut seen = std::collections::HashSet::new();
    for t in &file.starter {
        t.validate()?;
        if !seen.insert(t.id.clone()) {
            return Err(format!("starter id {:?} appears twice", t.id));
        }
    }
    Ok(file.starter)
}

/// The starter catalog, in file order.
pub fn templates() -> Vec<Template> {
    static CATALOG: std::sync::OnceLock<Vec<Template>> = std::sync::OnceLock::new();
    CATALOG
        .get_or_init(|| {
            parse_starters(crate::defaults::STARTERS_TOML).unwrap_or_else(|e| {
                tracing::error!("{e}");
                Vec::new()
            })
        })
        .clone()
}

pub fn template(id: &str) -> Option<Template> {
    templates().into_iter().find(|t| t.id == id)
}

/// What `Runtime::start_run` reads from the session's agent and freezes on
/// the run (§2): persona (into the prompt), model, tool scope and memory
/// scope. Editing the agent affects the next run, not one already working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frozen {
    pub agent_id: String,
    pub persona_path: PathBuf,
    /// The agent's model chain's head, when it sets one.
    pub model: Option<String>,
    /// `runs.tool_scope` as JSON; `None` = every tool.
    pub tool_scope: Option<String>,
    pub private_memory: bool,
    pub memory_path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtins() -> Vec<String> {
        [
            "read_file",
            "write_file",
            "shell",
            "http_fetch",
            "memory_save",
            "memory_search",
            "channel_read",
            "agent_spawn",
            "agent_ask_parent",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn scopes_parse_and_missing_keys_mean_all() {
        let s = ToolScope::parse(r#"{"builtins": "read_only", "mcp": ["browser"]}"#).unwrap();
        assert_eq!(s.builtins, Pick::ReadOnly);
        assert_eq!(s.connectors, Pick::All);
        assert_eq!(s.mcp, Pick::Names(vec!["browser".into()]));
        assert!(ToolScope::parse(r#"{"mcp": "read_only"}"#).is_err());
        assert!(ToolScope::parse(r#"{"nope": "all"}"#).is_err());
        assert_eq!(serde_json::to_string(&ToolScope::default()).unwrap(), "{}");
    }

    #[test]
    fn freezing_keeps_the_system_layer_and_patterns_for_sources() {
        let s = ToolScope::parse(r#"{"builtins": ["read_file"], "connectors": ["hubspot"], "mcp": [], "skills": ["pdf"]}"#)
            .unwrap();
        let f = s.freeze(&builtins());
        assert_eq!(
            f,
            [
                "channel_read",
                "lua__hubspot__*",
                "memory_save",
                "memory_search",
                "read_file",
                "skill:pdf"
            ]
        );
        let scope = Some(f.as_slice());
        assert!(crate::tool::in_scope(scope, "lua__hubspot__deals"));
        assert!(!crate::tool::in_scope(scope, "lua__gmail__send"));
        assert!(!crate::tool::in_scope(scope, "shell"));
        assert!(scope_allows_skill(scope, "pdf"));
        assert!(!scope_allows_skill(scope, "weekly"));
        // Read-only and everything: sub-agent-only tools never appear.
        let all = ToolScope::default().freeze(&builtins());
        assert!(
            all.contains(&"mcp__*".to_owned()) && !all.contains(&"agent_ask_parent".to_owned())
        );
        let ro = ToolScope {
            builtins: Pick::ReadOnly,
            ..ToolScope::default()
        }
        .freeze(&builtins());
        assert!(ro.contains(&"http_fetch".to_owned()) && !ro.contains(&"write_file".to_owned()));
        assert!(
            ToolScope::narrow(&builtins())
                .freeze(&builtins())
                .iter()
                .all(|n| n != "shell")
        );
    }

    #[test]
    fn slugs_and_paths() {
        assert_eq!(slug_for("Research Bot!"), "research-bot");
        assert_eq!(slug_for("  Ünïcode  "), "n-code");
        assert!(check_slug("dm").is_ok() && check_slug("-x").is_err());
        let home = TernHome::new("/t");
        assert_eq!(persona_path(&home, DEFAULT), PathBuf::from("/t/persona.md"));
        assert_eq!(
            persona_path(&home, "tern:research"),
            PathBuf::from("/t/agents/research/persona.md")
        );
        assert_eq!(
            memory_path(&home, "tern:research", false),
            PathBuf::from("/t/memory.md")
        );
        assert_eq!(
            memory_path(&home, "tern:research", true),
            PathBuf::from("/t/agents/research/memory.md")
        );
        assert_eq!(
            memory_path(&home, DEFAULT, true),
            PathBuf::from("/t/memory.md")
        );
    }

    #[test]
    fn router_answers_are_read_leniently_and_only_name_valid_agents() {
        let valid: Vec<String> = ["tern", "research", "ops"].map(String::from).to_vec();
        let pick = |t: &str| parse_router_answer(t, &valid);
        assert_eq!(
            pick(r#"{"agents": ["research"]}"#),
            Some(vec!["research".into()])
        );
        // Text around it, an @, case and repeats.
        assert_eq!(
            pick("Sure! {\"agents\": [\"@Ops\", \"ops\", \"research\"]} done"),
            Some(vec!["ops".into(), "research".into()])
        );
        // Unknown agents are dropped; nothing valid means no answer.
        assert_eq!(
            pick(r#"{"agents": ["ghost", "tern"]}"#),
            Some(vec!["tern".into()])
        );
        assert_eq!(pick(r#"{"agents": ["ghost"]}"#), None);
        assert_eq!(pick(r#"{"agents": []}"#), None);
        assert_eq!(pick("research"), None);
        assert_eq!(pick(r#"{"agents": "research"}"#), None);
        // At most three.
        let many: Vec<String> = (0..6).map(|i| format!("a{i}")).collect();
        let all = serde_json::json!({ "agents": many }).to_string();
        assert_eq!(
            parse_router_answer(&all, &many).unwrap().len(),
            ROUTER_MAX_AGENTS
        );
    }

    #[test]
    fn the_routing_prompt_lists_the_candidates_and_the_message() {
        let p = router_prompt(
            "hello",
            &[("tern".into(), "tern".into(), "the lead".into())],
        );
        assert!(p.contains("tern · tern · the lead") && p.ends_with("Message to route:\nhello"));
    }

    #[test]
    fn the_starter_catalog_parses_and_validates() {
        let all = parse_starters(crate::defaults::STARTERS_TOML).unwrap();
        assert!(all.len() >= 20, "{} starters", all.len());
        assert_eq!(all.len(), templates().len());
        for id in ["research", "inbox", "marketing", "blank"] {
            assert!(template(id).is_some(), "{id}");
        }
        assert!(template("nope").is_none());
        // Names (and so slugs) are unique, so every starter can be created.
        let mut slugs: Vec<_> = all
            .iter()
            .filter(|t| !t.name.is_empty())
            .map(|t| slug_for(&t.name))
            .collect();
        slugs.sort();
        let n = slugs.len();
        slugs.dedup();
        assert_eq!(n, slugs.len());
        // Categories are contiguous, so UIs can group by first appearance.
        let mut seen: Vec<&str> = Vec::new();
        for t in &all {
            if seen.last() != Some(&t.category.as_str()) {
                assert!(
                    !seen.contains(&t.category.as_str()),
                    "{} is split",
                    t.category
                );
                seen.push(&t.category);
            }
        }
        assert!(seen.len() >= 5 && seen.contains(&"Coding"));
    }

    #[test]
    fn starters_get_no_more_power_than_they_need() {
        for t in templates() {
            let Some(tools) = &t.tools else { continue };
            let names = match &tools.builtins {
                Pick::Names(n) => n.clone(),
                _ => Vec::new(),
            };
            let has = |x: &str| names.iter().any(|n| n == x);
            // Reviewers and summarizers read; they never edit.
            if ["code-reviewer", "security-reviewer", "pr-summarizer"].contains(&t.id.as_str()) {
                assert!(!has("write_file") && !has("edit_file"), "{}", t.id);
            }
            // Only coding starters run commands.
            if has("shell") {
                assert!(POWER_CATEGORIES.contains(&t.category.as_str()), "{}", t.id);
            }
            assert!(matches!(tools.mcp, Pick::Names(_)), "{}", t.id);
        }
    }

    #[test]
    fn the_seo_starter_is_a_read_only_scheduled_auditor() {
        use crate::approval::Autonomy;
        let t = template("seo").unwrap();
        assert!(t.validate().is_ok());
        assert_eq!(t.autonomy, Some(Autonomy::ReadOnly));
        assert_eq!(t.category, template("marketing").unwrap().category);
        let (cron, _) = t.routine.as_ref().expect("a suggested schedule");
        assert!(cron.split_whitespace().count() == 5);
        let tools = t.tools.as_ref().unwrap();
        assert!(matches!(tools.builtins, Pick::ReadOnly));
        for phrase in ["scorecard", "Changed since last run", "never edit", "llms.txt", "JSON-LD"] {
            assert!(t.persona.contains(phrase), "persona lacks {phrase:?}");
        }
    }

    #[test]
    fn validation_rejects_bad_starters() {
        let good = template("code-reviewer").unwrap();
        assert!(good.validate().is_ok());
        let mut t = good.clone();
        t.memory = "global".into();
        assert!(t.validate().is_err());
        let mut t = good.clone();
        t.avatar.1 = "mauve".into();
        assert!(t.validate().is_err());
        let mut t = good.clone();
        t.persona = "You are {name}.".into();
        assert!(t.validate().is_err());
        let mut t = good.clone();
        t.routine = Some(("not a cron".into(), "x".into()));
        assert!(t.validate().is_err());
        let mut t = good.clone();
        t.tools = Some(ToolScope {
            builtins: Pick::Names(vec!["rm_rf".into()]),
            ..ToolScope::default()
        });
        assert!(t.validate().is_err());
        let mut t = template("inbox").unwrap();
        t.tools = Some(ToolScope {
            builtins: Pick::Names(vec!["shell".into()]),
            ..ToolScope::default()
        });
        assert!(t.validate().is_err());
        let mut t = good.clone();
        t.category = " ".into();
        assert!(t.validate().is_err());
        // The ceiling: a real level, no higher than supervised, and readonly
        // only without write-capable tools.
        use crate::approval::Autonomy;
        let mut t = good.clone();
        t.autonomy = Some(Autonomy::Autonomous);
        assert!(t.validate().is_err());
        t.autonomy = Some(Autonomy::Yolo);
        assert!(t.validate().is_err());
        t.autonomy = Some(Autonomy::ReadOnly); // code-reviewer has shell
        assert!(t.validate().is_err());
        let mut t = template("research").unwrap();
        assert_eq!(t.autonomy, Some(Autonomy::ReadOnly));
        assert!(t.validate().is_ok());
        t.tools = None; // the narrow default writes files
        assert!(t.validate().is_err());
        let mut t = good;
        t.autonomy = Some(Autonomy::Supervised);
        assert!(t.validate().is_ok());
        // An unknown level fails to parse at all.
        let src = r#"[[starter]]
id = "x"
name = "X"
description = "d"
category = "c"
memory = "private"
autonomy = "loose"
avatar = ["●", "amber"]
persona = "You are X."
"#;
        assert!(parse_starters(src).is_err());
    }

    #[test]
    fn every_starter_has_a_conservative_ceiling() {
        use crate::approval::Autonomy;
        for t in templates() {
            let a = t
                .autonomy
                .unwrap_or_else(|| panic!("{} has no autonomy", t.id));
            assert!(a <= Autonomy::Supervised, "{}: {a}", t.id);
        }
        let level = |id: &str| template(id).unwrap().autonomy.unwrap();
        for id in ["research", "deep-research", "fact-checker", "meeting-prep", "seo"] {
            assert_eq!(level(id), Autonomy::ReadOnly, "{id}");
        }
        for id in ["inbox", "bug-fixer", "code-reviewer", "customer-support", "blank"] {
            assert_eq!(level(id), Autonomy::Supervised, "{id}");
        }
    }
}
