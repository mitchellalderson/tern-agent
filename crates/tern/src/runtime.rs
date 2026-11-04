//! Builds the agent from config: store, providers, tools, journal.

use std::path::PathBuf;
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use anyhow::{Context, bail};
use tern_core::agent::{Agent, AgentConfig, ProviderFactory};
use tern_core::approval::ApprovalHandler;
use tern_core::config::{ConfineMode, ModelRef, SandboxBackendKind};
use tern_core::journal::Journal;
use tern_core::models::Change;
use tern_core::prompt::{AgentRole, SubagentPrompt};
use tern_core::subagent::HostSlot;
use tern_core::tool::ToolRegistry;
use tern_core::types::Provider;
use tern_core::{Config, SecretStatus, Secrets, TernHome};
use tern_providers::{MockProvider, PROVIDER_ENV};
use tern_store::Store;
use tokio::sync::broadcast;

use crate::providers::ModelCache;
use crate::subagents::{LocalDriver, Subagents};

/// Builds system prompts, for runs the user starts and for sub-agents.
pub struct PromptMaker {
    home: TernHome,
    workspace: PathBuf,
    /// Weak: the registry's `agent_*` tools reach this through their host.
    tools: Weak<ToolRegistry>,
    /// Set when sub-agents are enabled.
    sub: Option<SubagentPrompt>,
    max_depth: u32,
    memory: tern_core::memory::MemoryConfig,
}

impl PromptMaker {
    fn new(home: &TernHome, config: &Config, tools: &Arc<ToolRegistry>) -> Self {
        let s = &config.subagents;
        Self {
            home: home.clone(),
            workspace: config.workspace_dir(),
            tools: Arc::downgrade(tools),
            sub: s.enabled.then(|| SubagentPrompt {
                max_concurrent: s.max_concurrent,
                max_iterations: s.max_iterations,
                budget_tokens: s.budget_tokens,
                models: s
                    .models
                    .iter()
                    .map(|(alias, model)| format!("`{alias}` ({model})"))
                    .collect(),
                can_nest: s.max_depth >= 2,
            }),
            max_depth: s.max_depth,
            memory: config.memory.clone(),
        }
    }

    /// The prompt for a run at `depth` (0 = the user's), frozen on its row.
    /// `agent` is the run's profile (`None`: the default agent's files);
    /// `scope` its tool scope, which limits the skills it lists and whether
    /// it hears about `python`.
    pub fn build(
        &self,
        role: AgentRole<'_>,
        depth: u32,
        agent: Option<&tern_core::prompt::AgentPrompt>,
        scope: Option<&tern_core::agents::ToolScope>,
    ) -> String {
        let skills: Vec<(String, String)> = tern_core::skills::discover(&self.home.skills_dir())
            .into_iter()
            .filter(|s| scope.is_none_or(|scope| scope.allows_skill(&s.name)))
            .map(|s| (s.name, s.description))
            .collect();
        let tools = self.tools.upgrade();
        let integrations = tools
            .as_ref()
            .map(|t| t.describe_sources())
            .unwrap_or_default();
        let mut prompt = tern_core::prompt::system_prompt(&tern_core::prompt::PromptInputs {
            home: &self.home,
            role,
            // Only runs that get the agent_* tools hear about them.
            subagents: self.sub.as_ref().filter(|_| depth < self.max_depth),
            now: chrono::Local::now(),
            workspace: &self.workspace,
            skills: &skills,
            integrations: &integrations,
            memory: &self.memory,
            agent,
        });
        if tools.is_some_and(|t| t.get(tern_core::script::PYTHON).is_some())
            && scope.is_none_or(|s| s.allows_builtin(tern_core::script::PYTHON))
        {
            prompt.push_str(tern_core::prompt::COMPUTE_NOTE);
        }
        prompt
    }
}

pub struct Runtime {
    pub home: TernHome,
    /// The config as loaded at startup. Providers and `default_model` can
    /// change while running; read those from [`Runtime::live`].
    pub config: Config,
    /// The latest `config.toml`, swapped by [`Runtime::reload_providers`].
    live: Arc<RwLock<Config>>,
    pub(crate) models: Arc<ModelCache>,
    pub(crate) changes: broadcast::Sender<Change>,
    pub store: Store,
    pub agent: Arc<Agent>,
    /// Set when `[subagents].enabled`. Drives children on local tasks
    /// until the daemon hands it its `Manager`.
    pub subagents: Option<Arc<Subagents>>,
    prompts: Arc<PromptMaker>,
    /// Shared with providers, connectors and MCP servers: edits through it
    /// apply on their next use.
    pub secrets: Secrets,
    connectors: Arc<tern_lua::Connectors>,
    /// The live MCP server set and its config writes.
    pub(crate) mcp_admin: Arc<crate::mcp::McpAdmin>,
    /// Accounts of connectors and MCP servers (tooling spec §6.8).
    pub connections: Arc<tern_auth::Connections>,
    /// What the auth tools share (plans, clients), and what accounts can be
    /// added to: for front ends adding accounts themselves.
    pub auth: Arc<tern_auth::tools::AuthHost>,
    pub auth_targets: tern_auth::tools::Targets,
    /// Set when `TERN_PROVIDER=mock:<script>` overrides real providers.
    pub mock: bool,
}

impl Runtime {
    pub fn open(home: &TernHome, approvals: Arc<dyn ApprovalHandler>) -> anyhow::Result<Self> {
        home.ensure_layout()
            .with_context(|| format!("creating {}", home.root().display()))?;
        let (config, _) = Config::load(&home.config_path())?;
        let secrets = Secrets::load(&home.secrets_path())?;
        let store = Store::open(&home.db_path())
            .with_context(|| format!("opening {}", home.db_path().display()))?;
        crate::ledger_cmd::note_config(&store, &config);

        let workspace = config.workspace_dir();
        std::fs::create_dir_all(&workspace)
            .with_context(|| format!("creating workspace {}", workspace.display()))?;

        let live = Arc::new(RwLock::new(config.clone()));
        let (providers, mock) = provider_factory(live.clone(), &secrets)?;
        let changes = broadcast::channel(64).0;
        let models = Arc::new(ModelCache::load(
            home.root().join("cache").join("models.json"),
            changes.clone(),
        ));
        let slot = HostSlot::default();
        let connections = open_connections(&config, &store, &secrets);
        let (mut tools, connectors, mcp) = build_tools(
            home,
            &config,
            &store,
            &secrets,
            &slot,
            &changes,
            &connections,
        )?;
        // Channels (docs/channels-design.md §3): a post an agent writes is
        // announced like the daemon's own.
        let post_changes = changes.clone();
        tools.register(Arc::new(tern_tools::channels::ChannelRead::new(
            store.clone(),
        )));
        let announce = |changes: tokio::sync::broadcast::Sender<Change>,
                        store: tern_store::Store| {
            Arc::new(move |row: tern_store::PostRow| {
                let suggestion = tern_core::channels::suggestion_of(&store, &row).map(Box::new);
                let _ = changes.send(Change::PostCreated {
                    post: Box::new(tern_core::channels::Post {
                        suggestion,
                        ..tern_core::channels::Post::from(row)
                    }),
                });
            }) as tern_tools::channels::OnPost
        };
        tools.register(Arc::new(tern_tools::channels::ChannelPost::new(
            store.clone(),
            Some(announce(post_changes.clone(), store.clone())),
        )));
        tools.register(Arc::new(tern_tools::channels::ChannelSuggest::new(
            store.clone(),
            Some(announce(post_changes, store.clone())),
        )));
        tools.register(Arc::new(tern_tools::secret_request::SecretRequest::new(
            approvals.clone(),
            secrets.clone(),
            store.clone(),
            secret_uses(home, live.clone(), &secrets, &store)?,
            Duration::from_secs(config.secret_request_timeout_secs),
        )));
        // Connecting accounts (tooling spec §6.7.4).
        // One host for the three, so a plan found by auth_discover is the
        // one auth_propose shows and connection_add uses.
        let auth_host = Arc::new(tern_auth::tools::AuthHost::new(&config.auth));
        tools.register(Arc::new(tern_auth::tools::AuthDiscover {
            host: auth_host.clone(),
        }));
        let targets = auth_targets(home, live.clone(), &secrets, &store)?;
        tools.register(Arc::new(tern_auth::tools::AuthPropose {
            host: auth_host.clone(),
            asker: approvals.clone(),
            targets: targets.clone(),
        }));
        tools.register(Arc::new(tern_auth::tools::ConnectionAdd::new(
            auth_host.clone(),
            connections.clone(),
            approvals.clone(),
            targets.clone(),
        )));
        let conn_changes = changes.clone();
        connections.on_change(move || {
            let _ = conn_changes.send(Change::ConnectionsChanged {});
        });
        let mcp_admin = Arc::new(crate::mcp::McpAdmin {
            home: home.clone(),
            live: live.clone(),
            secrets: secrets.clone(),
            mcp,
            connectors: connectors.clone(),
        });
        crate::mcp_tools::register(&mut tools, mcp_admin.clone());
        let tools = Arc::new(tools);

        // Every value in secrets.toml, plus env-provided secrets the config
        // or a connector names.
        let mut names = config.secret_names();
        names.extend(connectors.secret_names());
        let mut redactions = secrets.file_values();
        redactions.extend(names.iter().filter_map(|n| secrets.get(n)));
        let journal = Journal::new(store.clone())
            .with_redactions(redactions)
            .with_secrets(secrets.clone())
            .with_crash_after_from_env();

        let agent = Agent::new(
            journal.clone(),
            providers,
            tools.clone(),
            approvals,
            AgentConfig {
                workspace: canonical(workspace),
                max_iterations: config.max_iterations,
                default_context_window: config.context_window,
                context_windows: config
                    .providers
                    .iter()
                    .filter_map(|(name, p)| p.context_window.map(|w| (name.clone(), w)))
                    .collect(),
                memory_flush: config.memory.flush,
                approval_wait: std::time::Duration::from_secs(config.outbox.wait_secs),
                outbox_expiry: std::time::Duration::from_secs(
                    config.outbox.expire_hours.saturating_mul(3600),
                ),
                ..AgentConfig::default()
            },
        )
        .with_subagents(slot.clone());
        let agent = Arc::new(agent);
        let prompts = Arc::new(PromptMaker::new(home, &config, &tools));
        let subagents = config.subagents.enabled.then(|| {
            let host = Arc::new(Subagents::new(
                store.clone(),
                journal,
                agent.event_sender(),
                Arc::downgrade(&tools),
                &config,
                live.clone(),
                mock,
            ));
            host.set_driver(Arc::new(LocalDriver::new(&agent)));
            let p = prompts.clone();
            let st = store.clone();
            let h = home.clone();
            host.set_prompt(Box::new(move |label, depth, agent, caller| {
                // A helper works under its agent's persona and memory: its
                // parent's, or the profile it was delegated to.
                let frame = st
                    .agent(agent)
                    .ok()
                    .flatten()
                    .map(|a| agent_frame(&h, &st, &a));
                let (prompt, scope) = match &frame {
                    Some((prompt, scope)) => (prompt.as_ref(), scope.as_ref()),
                    None => (None, None),
                };
                p.build(AgentRole::Subagent { label, caller }, depth, prompt, scope)
            }));
            slot.set(host.clone());
            host
        });
        if let Some(h) = &subagents
            && let Err(e) = h.reconcile()
        {
            tracing::warn!(error = %e, "reconciling sub-agent reports failed");
        }
        let rt = Self {
            home: home.clone(),
            config,
            live,
            models,
            changes,
            store,
            agent,
            subagents,
            prompts,
            secrets,
            connectors,
            mcp_admin,
            connections,
            auth: auth_host,
            auth_targets: targets,
            mock,
        };
        rt.refresh_agent_chains();
        Ok(rt)
    }

    /// Create an agent profile with its persona file and DM. Fields left
    /// out come from `template` (if any), else the defaults: private
    /// memory, the narrow tool scope, the blank persona.
    pub fn create_agent(&self, a: NewAgent) -> anyhow::Result<tern_store::AgentRow> {
        use tern_core::agents;
        let template = match a.template.as_deref() {
            Some(t) => Some(agents::template(t).with_context(|| format!("no template {t:?}"))?),
            None => None,
        };
        let name = a.name.trim().to_owned();
        if name.is_empty() {
            bail!("name is empty");
        }
        let slug = match a.slug.as_deref().map(|s| s.trim().trim_start_matches('@')) {
            Some(s) if !s.is_empty() => s.to_owned(),
            _ => agents::slug_for(&name),
        };
        agents::check_slug(&slug).map_err(anyhow::Error::msg)?;
        if self.store.agent(&slug)?.is_some() {
            bail!("there is already an agent @{slug}");
        }
        if self.store.channel(&tern_store::dm_slug(&slug))?.is_some() {
            bail!("the channel #{} is taken", tern_store::dm_slug(&slug));
        }
        let mut id = agents::id_for(&slug);
        match self.store.agent_or_deleted(&id)? {
            None => {}
            Some(old) if old.deleted_at.is_none() => {
                bail!(
                    "an archived agent already has the id {id}; unarchive it or pick another name"
                );
            }
            // A deleted agent keeps its id (history names it): the new
            // agent gets the next free one.
            Some(_) => {
                let mut n = 2;
                while self
                    .store
                    .agent_or_deleted(&agents::id_for(&format!("{slug}-{n}")))?
                    .is_some()
                {
                    n += 1;
                }
                id = agents::id_for(&format!("{slug}-{n}"));
            }
        }
        let memory = a
            .memory
            .or_else(|| template.as_ref().map(|t| t.memory.to_owned()))
            .unwrap_or_else(|| "private".into());
        if !matches!(memory.as_str(), "shared" | "private") {
            bail!("memory must be \"shared\" or \"private\", not {memory:?}");
        }
        let tools = a
            .tools
            .or_else(|| template.as_ref().and_then(|t| t.tools.clone()))
            .unwrap_or_else(|| agents::ToolScope::narrow(&self.agent.tools().builtin_names()));
        let avatar = a.avatar.or_else(|| {
            template.as_ref().map(|t| {
                serde_json::json!({ "glyph": t.avatar.0, "color": t.avatar.1 }).to_string()
            })
        });
        let persona = a
            .persona
            .filter(|p| !p.trim().is_empty())
            .or_else(|| template.as_ref().map(|t| t.persona.to_owned()))
            .or_else(|| agents::template("blank").map(|t| t.persona.to_owned()))
            .unwrap_or_default()
            .replace("{name}", &name);
        self.write_persona(&id, &persona)?;
        let (row, _) = self.store.create_agent(&tern_store::AgentRow {
            id,
            slug,
            name,
            avatar,
            model: a.model,
            subagent_model: a.subagent_model,
            tools: Some(serde_json::to_string(&tools)?),
            memory,
            egress: a.egress.map(|e| e.as_str().to_owned()),
            autonomy: a.autonomy.map(|e| e.as_str().to_owned()),
            ..tern_store::AgentRow::default()
        })?;
        self.refresh_agent_chains();
        Ok(row)
    }

    /// Write an agent's `persona.md`, atomically.
    pub fn write_persona(&self, id: &str, text: &str) -> anyhow::Result<()> {
        let path = tern_core::agents::persona_path(&self.home, id);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = path.with_extension("md.tmp");
        std::fs::write(&tmp, format!("{}\n", text.trim()))
            .and_then(|()| std::fs::rename(&tmp, &path))
            .with_context(|| format!("writing {}", path.display()))
    }

    /// What tern adds under `agent`'s persona in a run's prompt: everything
    /// after the persona, for the editor's read-only preview.
    pub fn system_sections(&self, agent: &tern_store::AgentRow) -> String {
        let (prompt, scope) = agent_frame(&self.home, &self.store, agent);
        let full = self
            .prompts
            .build(AgentRole::Main, 0, prompt.as_ref(), scope.as_ref());
        let persona =
            std::fs::read_to_string(tern_core::agents::persona_path(&self.home, &agent.id))
                .ok()
                .map(|t| tern_core::memory::injectable(&t))
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| tern_core::defaults::PERSONA_MD.trim().to_owned());
        full.strip_prefix(persona.as_str())
            .unwrap_or(&full)
            .trim_start()
            .to_owned()
    }

    /// Put the agents' model chains into the live config, so a run on an
    /// agent's model gets that agent's fallbacks (T6a's head-model rule).
    pub fn refresh_agent_chains(&self) {
        let chains = agent_chains(&self.store);
        match self.live.write() {
            Ok(mut g) => g.agent_chains = chains,
            Err(poisoned) => poisoned.into_inner().agent_chains = chains,
        }
    }

    /// Every secret in `secrets.toml` or referred to by config or an active
    /// connector, with what uses it. Never includes values.
    pub fn secret_statuses(&self) -> Vec<SecretStatus> {
        let mut uses = self.live().secret_uses();
        uses.extend(self.connector_secret_uses());
        self.secrets.statuses(&uses)
    }

    pub(crate) fn connector_secret_uses(&self) -> Vec<(String, String)> {
        self.connectors.secret_uses()
    }

    /// The latest config: providers and `default_model` as they are now.
    pub fn live(&self) -> Config {
        match self.live.read() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub(crate) fn set_live(&self, mut config: Config) {
        config.agent_chains = agent_chains(&self.store);
        crate::ledger_cmd::note_config(&self.store, &config);
        match self.live.write() {
            Ok(mut g) => *g = config,
            Err(poisoned) => *poisoned.into_inner() = config,
        }
    }

    /// Provider and model-list changes, for `/v1/events`.
    pub fn subscribe_changes(&self) -> broadcast::Receiver<Change> {
        self.changes.subscribe()
    }

    /// `--model` (resolved, so the provider may be left off), else the
    /// session's model, else `default_model`.
    pub fn pick_model(
        &self,
        flag: Option<&str>,
        session_model: Option<&str>,
    ) -> anyhow::Result<ModelRef> {
        self.pick_model_for(flag, session_model, None)
    }

    /// [`Runtime::pick_model`] with the agent's model before `default_model`.
    fn pick_model_for(
        &self,
        flag: Option<&str>,
        session_model: Option<&str>,
        agent_model: Option<&str>,
    ) -> anyhow::Result<ModelRef> {
        if let Some(m) = flag {
            return Ok(self.resolve_model(m)?.model.parse()?);
        }
        if let Some(m) = session_model {
            return Ok(m.parse()?);
        }
        if let Some(m) = agent_model {
            return Ok(m.parse()?);
        }
        match self.live().default_model_ref() {
            Some(m) => Ok(m?),
            None if self.mock => Ok("mock:mock".parse()?),
            None => bail!(
                "no model selected: run `tern setup` to connect a provider, or pass --model provider:model"
            ),
        }
    }

    /// A Luau host over this runtime's store and secrets, for watches and
    /// gates (T1b).
    pub fn lua_host(&self) -> anyhow::Result<tern_lua::Host> {
        crate::routines::lua_host(&self.store, &self.secrets)
    }

    /// Checks for routines' watches against the active connectors.
    pub fn routine_checks(&self) -> anyhow::Result<crate::routines::LuaChecks> {
        Ok(crate::routines::LuaChecks::new(
            self.lua_host()?,
            self.home.connectors_dir(),
        ))
    }

    /// Check that `model` resolves to a configured provider.
    /// The "Where you are" section of a run's frozen prompt: the place,
    /// where its reply goes, who else is there, the channel's
    /// instructions; for a routine, where its result goes. `None` for a
    /// session in no channel.
    fn where_section(
        &self,
        session: &tern_store::SessionRow,
        agent_id: &str,
        routine: bool,
    ) -> anyhow::Result<Option<String>> {
        use tern_core::channels::{MemberLine, Place, routine_where_section, where_section};
        if routine && let Some(s) = self.store.schedule_for_session(&session.id)? {
            let target = s.channel_id.as_deref().unwrap_or(tern_store::HOME);
            let posted_to = match self.store.channel(target)? {
                Some(c) => self.place_name(&c)?,
                None => "Home".into(),
            };
            let delivered = s
                .deliver_to
                .as_deref()
                .map(|t| t.split(':').next().unwrap_or(t))
                .map(|g| {
                    let mut c = g.chars();
                    c.next()
                        .map(|f| f.to_uppercase().chain(c).collect::<String>())
                        .unwrap_or_default()
                });
            return Ok(Some(routine_where_section(
                &crate::daemon::scheduler::title(&s),
                &posted_to,
                delivered.as_deref(),
            )));
        }
        let Some(c) = self.store.session_channel(&session.id)? else {
            return Ok(None);
        };
        let owner = if c.kind == "main" {
            self.store.agent(c.lead())?
        } else {
            None
        };
        let place = match (&*c.kind, &owner) {
            ("topic", _) => Place::Topic {
                slug: &c.slug,
                title: &c.title,
            },
            (_, Some(a)) if c.id != tern_store::HOME => Place::Dm(&a.name),
            _ => Place::Home,
        };
        let mut agents = Vec::new();
        if c.kind == "topic" {
            for m in self.store.channel_members(&c)? {
                if let Some(a) = self.store.agent(&m.agent_id)? {
                    agents.push((a, m.lead));
                }
            }
        }
        let lines: Vec<_> = agents
            .iter()
            .map(|(a, lead)| MemberLine {
                slug: &a.slug,
                name: &a.name,
                lead: *lead,
                you: a.id == agent_id,
            })
            .collect();
        let wakes = self.store.channel_wakes(&c.id)?.agent_wakes;
        Ok(Some(where_section(
            &place,
            &lines,
            wakes,
            c.instructions.as_deref(),
        )))
    }

    /// A channel as a place in a sentence: `#slug`, Home, `Name's DM`.
    pub fn place_name(&self, c: &tern_store::ChannelRow) -> anyhow::Result<String> {
        use tern_core::channels::Place;
        Ok(if c.kind == "topic" {
            Place::Topic {
                slug: &c.slug,
                title: &c.title,
            }
            .name()
        } else if c.id == tern_store::HOME {
            Place::Home.name()
        } else {
            match self.store.agent(c.lead())? {
                Some(a) => Place::Dm(&a.name).name(),
                None => c.title.clone(),
            }
        })
    }

    /// The autonomy a run of `agent` gets: `requested` (a routine's own) or
    /// else the configured one, capped by the agent's ceiling. `Autonomy`
    /// is ordered, so the cap is a `min`.
    pub fn autonomy_for(
        &self,
        agent: &tern_store::AgentRow,
        requested: Option<tern_core::approval::Autonomy>,
    ) -> tern_core::approval::Autonomy {
        let base = requested.unwrap_or(self.live().autonomy);
        match agent
            .autonomy
            .as_deref()
            .and_then(tern_core::approval::Autonomy::parse)
        {
            Some(ceiling) => base.min(ceiling),
            None => base,
        }
    }

    pub fn check_model(&self, model: &str) -> anyhow::Result<ModelRef> {
        Ok(self.resolve_model(model)?.model.parse()?)
    }

    /// Record a run of `text`. Uses `session` (or creates one from `source`),
    /// picks the model, and freezes the system prompt on the run.
    pub fn start_run(
        &self,
        session: Option<&str>,
        model: Option<&str>,
        text: &str,
        source: &str,
        trigger: &str,
    ) -> anyhow::Result<StartedRun> {
        self.start_run_tainted(session, model, text, source, trigger, None)
    }

    /// [`Runtime::start_run`] for untrusted input: the run starts tainted
    /// for `taint` (tooling spec §5.3), e.g. a routine started by a watch.
    pub fn start_run_tainted(
        &self,
        session: Option<&str>,
        model: Option<&str>,
        text: &str,
        source: &str,
        trigger: &str,
        taint: Option<&str>,
    ) -> anyhow::Result<StartedRun> {
        let session = match session {
            Some(id) => self
                .store
                .session(id)?
                .with_context(|| format!("session {id} not found"))?,
            None => self
                .store
                .create_session(&tern_core::agent::new_id(), source, None)?,
        };
        // The session's agent, read once: persona, model, tool scope and
        // memory scope are frozen on this run (agent profiles §2).
        let agent = match self.store.agent(session.agent())? {
            Some(a) => a,
            None => match self.store.agent_or_deleted(session.agent())? {
                Some(gone) => bail!(
                    "{} was deleted; its conversations stay readable but can't be continued",
                    gone.name
                ),
                None => bail!("agent {} not found", session.agent()),
            },
        };
        if agent.archived_at.is_some() {
            bail!("{} is archived", agent.name);
        }
        let agent_model = agent
            .model
            .as_deref()
            .and_then(tern_core::config::ModelChain::from_stored)
            .map(|c| c.primary().to_owned());
        let model = self.pick_model_for(model, session.model.as_deref(), agent_model.as_deref())?;
        // An agent's own model isn't pinned on the session, so editing the
        // agent reaches its DM's next run; a chosen one (`--model`, `/model`)
        // or the default is, as before.
        let from_agent =
            session.model.is_none() && agent_model.as_deref() == Some(model.to_string().as_str());
        if !from_agent && session.model.as_deref() != Some(&model.to_string()) {
            self.store
                .set_session_model(&session.id, &model.to_string())?;
        }
        let role = if trigger == "cron" {
            AgentRole::Scheduled
        } else {
            AgentRole::Main
        };
        let (prompt, scope) = agent_frame(&self.home, &self.store, &agent);
        let mut system = self.prompts.build(role, 0, prompt.as_ref(), scope.as_ref());
        // Always, so the agent knows where it is and that its reply is
        // delivered there; frozen with the prompt, so replay sees the same.
        if let Some(section) = self.where_section(&session, &agent.id, trigger == "cron")? {
            system.push_str(&section);
        }
        let tool_scope = scope
            .map(|s| s.freeze(&self.agent.tools().builtin_names()))
            .map(|names| serde_json::to_string(&names))
            .transpose()?;
        let run = self.agent.start_run_as(
            &session.id,
            text,
            &model,
            &system,
            trigger,
            tool_scope.as_deref(),
            agent.private_memory().then_some("private"),
            taint,
        )?;
        // Frozen on the run, like its model (tooling spec §5.4); a routine
        // overrides it in `Manager::start_scheduled`, and the agent's
        // ceiling caps either (agent profiles AP3).
        self.store
            .set_run_autonomy(&run.id, self.autonomy_for(&agent, None).as_str())?;
        // The tainted host limit: the agent's own setting, else the
        // config's; frozen like autonomy.
        let egress = agent
            .egress
            .as_deref()
            .and_then(tern_core::approval::Egress::parse)
            .unwrap_or(self.live().taint.egress);
        self.store.set_run_egress(&run.id, egress.as_str())?;
        Ok(StartedRun {
            session_id: session.id,
            run_id: run.id,
            model,
        })
    }
}

/// A new agent profile ([`Runtime::create_agent`]). `model` and
/// `subagent_model` are already checked and in stored form.
#[derive(Debug, Default)]
pub struct NewAgent {
    pub name: String,
    pub slug: Option<String>,
    /// JSON `{glyph, color}`.
    pub avatar: Option<String>,
    pub persona: Option<String>,
    pub model: Option<String>,
    pub subagent_model: Option<String>,
    pub tools: Option<tern_core::agents::ToolScope>,
    pub memory: Option<String>,
    /// The tainted host limit for its runs; `None` = `[taint] egress`.
    pub egress: Option<tern_core::approval::Egress>,
    /// The most autonomy its runs may have; `None` = no ceiling.
    pub autonomy: Option<tern_core::approval::Autonomy>,
    pub template: Option<String>,
}

pub struct StartedRun {
    pub session_id: String,
    pub run_id: String,
    pub model: ModelRef,
}

/// What the prompt and the tool scope take from `agent`: `None` for the
/// default agent with everything, so its runs are what they were before
/// profiles.
pub(crate) fn agent_frame(
    home: &TernHome,
    store: &Store,
    agent: &tern_store::AgentRow,
) -> (
    Option<tern_core::prompt::AgentPrompt>,
    Option<tern_core::agents::ToolScope>,
) {
    let scope = agent
        .tools
        .as_deref()
        .map(|t| match tern_core::agents::ToolScope::parse(t) {
            Ok(s) => s,
            Err(e) => {
                // Fail closed: read-only built-ins, nothing else.
                tracing::warn!(agent = %agent.id, error = %e, "ignoring a bad tool scope; using read-only tools");
                tern_core::agents::ToolScope {
                    builtins: tern_core::agents::Pick::ReadOnly,
                    connectors: tern_core::agents::Pick::Names(Vec::new()),
                    mcp: tern_core::agents::Pick::Names(Vec::new()),
                    skills: tern_core::agents::Pick::Names(Vec::new()),
                }
            }
        });
    let others = other_agents(home, store, &agent.id);
    if agent.is_default() && others.is_empty() {
        return (None, scope);
    }
    let delegates = store
        .agents(false)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| agent.may_delegate_to(a))
        .map(|a| {
            let job = use_for(home, &a);
            format!("`{}` · {} · use for: {job}", a.slug, a.name)
        })
        .collect();
    let prompt = tern_core::prompt::AgentPrompt {
        persona_path: tern_core::agents::persona_path(home, &agent.id),
        memory_path: tern_core::agents::memory_path(home, &agent.id, agent.private_memory()),
        others,
        delegates,
    };
    (Some(prompt), scope)
}

/// What an agent is for, from its persona: the "Your job: …" line the
/// templates write, else its first line that isn't "You are …".
fn use_for(home: &TernHome, a: &tern_store::AgentRow) -> String {
    let text = std::fs::read_to_string(tern_core::agents::persona_path(home, &a.id))
        .map(|t| tern_core::memory::injectable(&t))
        .unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim().trim_start_matches(['-', '*', ' ']))
        .filter(|l| !l.is_empty())
        .collect();
    let line = lines
        .iter()
        .find_map(|l| {
            let i = l.to_ascii_lowercase().find("job:")?;
            Some(l[i + 4..].trim())
        })
        .or_else(|| lines.iter().copied().find(|l| !l.starts_with("You are")))
        .or_else(|| lines.first().copied())
        .unwrap_or("its own job (see its name)");
    line.chars().take(160).collect()
}

/// One line per other active agent: `@slug · Name · first line of its persona`.
fn other_agents(home: &TernHome, store: &Store, me: &str) -> Vec<String> {
    store
        .agents(false)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.id != me)
        .map(|a| format!("@{} · {} · {}", a.slug, a.name, persona_line(home, &a.id)))
        .collect()
}

/// The first line of an agent's persona, as a short description of it.
pub(crate) fn persona_line(home: &TernHome, agent_id: &str) -> String {
    std::fs::read_to_string(tern_core::agents::persona_path(home, agent_id))
        .ok()
        .and_then(|t| {
            t.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with("<!--"))
                .map(|l| l.chars().take(120).collect::<String>())
        })
        .unwrap_or_default()
}

/// `(slug, chain)` for every active agent that sets a model.
fn agent_chains(store: &Store) -> Vec<(String, tern_core::config::ModelChain)> {
    store
        .agents(false)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|a| {
            let chain = tern_core::config::ModelChain::from_stored(a.model.as_deref()?)?;
            Some((a.slug, chain))
        })
        .collect()
}

fn canonical(p: PathBuf) -> PathBuf {
    std::fs::canonicalize(&p).unwrap_or(p)
}

/// Reads providers from `live` on every call, so a reload applies to the
/// next model call.
fn provider_factory(
    live: Arc<RwLock<Config>>,
    secrets: &Secrets,
) -> anyhow::Result<(ProviderFactory, bool)> {
    if let Some(spec) = std::env::var(PROVIDER_ENV).ok().filter(|v| !v.is_empty()) {
        let Some(path) = spec.strip_prefix("mock:") else {
            bail!("{PROVIDER_ENV} must be `mock:<script.json>`, got {spec:?}");
        };
        let mock: Arc<dyn Provider> = Arc::new(MockProvider::from_file(path.as_ref())?);
        // Every model is the mock, but chains still apply, so fallback
        // tests run through the real chain.
        let factory: ProviderFactory = Arc::new(move |model: &ModelRef| {
            let config = live.read().unwrap_or_else(|p| p.into_inner());
            tern_providers::with_fallbacks(model, &config, |_| Ok(mock.clone()))
        });
        return Ok((factory, true));
    }
    let secrets = secrets.clone();
    let factory: ProviderFactory = Arc::new(move |model: &ModelRef| {
        let config = live.read().unwrap_or_else(|p| p.into_inner());
        tern_providers::for_model(model, &config, &secrets)
    });
    Ok((factory, false))
}

/// What accounts can be added to: every connector (active or draft) and
/// MCP server, with the secrets and OAuth provider each names.
pub(crate) fn auth_targets(
    home: &TernHome,
    live: Arc<RwLock<Config>>,
    secrets: &Secrets,
    store: &Store,
) -> anyhow::Result<tern_auth::tools::Targets> {
    let lua = tern_lua::Host::new(store.clone(), secrets.clone())?;
    let (active, drafts) = (home.connectors_dir(), home.drafts_dir());
    Ok(Arc::new(move || {
        use tern_auth::tools::Target;
        let mut out: Vec<Target> = Vec::new();
        for dir in [&active, &drafts] {
            for path in crate::connectors_cmd::luau_files_in(dir) {
                let Ok(vm) = tern_lua::load_file(&lua, &path) else {
                    continue;
                };
                let c = &vm.spec.capabilities;
                if out
                    .iter()
                    .any(|t| t.name == vm.spec.name && t.kind == tern_auth::Kind::Connector)
                {
                    continue;
                }
                out.push(Target {
                    kind: tern_auth::Kind::Connector,
                    name: vm.spec.name.clone(),
                    key_names: c.all_secrets(),
                    oauth: c
                        .oauth
                        .as_ref()
                        .map(|o| (o.provider.clone(), o.scopes.clone())),
                    hosts: c.http.clone(),
                });
            }
        }
        if let Ok(cfg) = live.read() {
            for (name, s) in &cfg.mcp.servers {
                out.push(Target {
                    kind: tern_auth::Kind::Mcp,
                    name: name.clone(),
                    key_names: s.secret_names().map(str::to_owned).collect(),
                    oauth: None,
                    hosts: s
                        .url
                        .as_deref()
                        .and_then(|u| reqwest::Url::parse(u).ok())
                        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
                        .into_iter()
                        .collect(),
                });
            }
        }
        out
    }))
}

/// What reads each secret, for `secret_request` prompts: config entries plus
/// every connector, active or draft, with the hosts it reaches.
fn secret_uses(
    home: &TernHome,
    live: Arc<RwLock<Config>>,
    secrets: &Secrets,
    store: &Store,
) -> anyhow::Result<tern_tools::secret_request::SecretUses> {
    let lua = tern_lua::Host::new(store.clone(), secrets.clone())?;
    let (active, drafts) = (home.connectors_dir(), home.drafts_dir());
    Ok(Arc::new(move || {
        let mut uses = live.read().map(|c| c.secret_uses()).unwrap_or_default();
        uses.extend(tern_lua::describe_secret_uses(&lua, &active, &drafts));
        uses
    }))
}

/// The host backend's sandbox from `[sandbox]`. tern's data dir is never
/// reachable from a shell command.
pub(crate) fn host_policy(config: &Config, home: &TernHome) -> tern_sandbox::HostPolicy {
    let sb = &config.sandbox;
    tern_sandbox::HostPolicy {
        confine: match sb.confine {
            ConfineMode::Auto => tern_sandbox::Confine::Auto,
            ConfineMode::Required => tern_sandbox::Confine::Required,
            ConfineMode::Off => tern_sandbox::Confine::Off,
        },
        network: sb.network,
        read_paths: sb.read_paths(),
        write_paths: sb.write_paths(),
        deny_paths: vec![home.root().to_path_buf()],
        helper: None,
    }
}

/// The `python` tool, its scripts run by `tern __monty-exec` under the
/// host sandbox with no network and nothing writable outside the usual
/// temp dirs and workspace (docs/python-tool-spec.md §5.2), whichever
/// backend `shell` uses.
#[cfg(feature = "python")]
fn python_tool(config: &Config, home: &TernHome) -> tern_python::Python {
    let limits = tern_python::Limits::default();
    let Ok(program) = std::env::current_exe() else {
        tracing::warn!("python: can't find tern's own binary; scripts run in-process");
        return tern_python::Python::new(limits);
    };
    let mut policy = host_policy(config, home);
    policy.network = false;
    policy.write_paths.clear();
    if let Some(dir) = program.parent() {
        policy.read_paths.push(dir.to_path_buf());
    }
    let sandbox = (policy.confine != tern_sandbox::Confine::Off)
        .then(|| Arc::new(tern_sandbox::HostBackend::new(policy)));
    tern_python::Python::with_helper(limits, tern_python::Helper { program, sandbox })
}

fn docker_settings(config: &Config) -> tern_sandbox::DockerSettings {
    let sb = &config.sandbox;
    tern_sandbox::DockerSettings {
        image: sb.docker_image.clone(),
        network: sb.network,
        memory: Some(sb.docker_memory.clone()).filter(|m| !m.is_empty()),
        cpus: Some(sb.docker_cpus).filter(|c| *c > 0.0),
        pids: Some(sb.docker_pids).filter(|p| *p > 0),
    }
}

/// Accounts and their tokens, shared by connectors, MCP servers, the auth
/// tools and front ends.
pub fn open_connections(
    config: &Config,
    store: &Store,
    secrets: &Secrets,
) -> Arc<tern_auth::Connections> {
    let tokens = Arc::new(tern_auth::Tokens::new(
        secrets.clone(),
        tern_auth::Guard::new(&config.auth.local_issuers),
    ));
    Arc::new(tern_auth::Connections::new(
        store.clone(),
        tokens,
        config.auth.max_connections as usize,
    ))
}

fn build_tools(
    home: &TernHome,
    config: &Config,
    store: &Store,
    secrets: &Secrets,
    subagents: &HostSlot,
    changes: &broadcast::Sender<Change>,
    connections: &Arc<tern_auth::Connections>,
) -> anyhow::Result<(
    ToolRegistry,
    Arc<tern_lua::Connectors>,
    Arc<tern_mcp::McpServers>,
)> {
    // MCP state and routine changes are announced on `/v1/events`.
    let mcp_changes = changes.clone();
    let mcp_notify: tern_mcp::Notify = Arc::new(move |name, state| {
        let _ = mcp_changes.send(Change::McpChanged {
            name: name.to_owned(),
            state,
        });
    });
    let schedule_changes = changes.clone();
    let schedules: tern_tools::schedule::Notify = Arc::new(move |id| {
        let _ = schedule_changes.send(Change::SchedulesChanged { id: id.to_owned() });
    });
    let sandbox: Arc<dyn tern_sandbox::SandboxBackend> = match config.sandbox.backend {
        SandboxBackendKind::Host => {
            Arc::new(tern_sandbox::HostBackend::new(host_policy(config, home)))
        }
        SandboxBackendKind::Docker => Arc::new(tern_sandbox::DockerBackend::new(
            "docker",
            &config.workspace_dir(),
            docker_settings(config),
        )),
    };
    let mut registry = ToolRegistry::new();
    tern_tools::register_builtins(
        &mut registry,
        tern_tools::BuiltinOptions {
            store: store.clone(),
            guard: tern_tools::paths::Guard::for_data_dir(home.root()),
            skills_dir: home.skills_dir(),
            sandbox,
            shell_timeout: Duration::from_secs(config.sandbox.shell_timeout_secs),
        },
    )
    .map_err(anyhow::Error::msg)?;
    registry.register(Arc::new(
        tern_tools::memory::ProfileEdit::new(home.clone(), config.memory.clone())
            .with_store(store.clone()),
    ));
    if config.subagents.enabled && config.subagents.max_depth >= 1 {
        tern_tools::agents::register(&mut registry, subagents.clone());
    }
    #[cfg(feature = "python")]
    registry.register(Arc::new(python_tool(config, home)));

    let mut lua = tern_lua::Host::new(store.clone(), secrets.clone())?;
    lua.default_rate = config.connectors.rate;
    lua.record_fixtures = config.connectors.record_tests;
    lua.connections = Some(connections.clone());
    let dirs = tern_lua::tools::Dirs {
        active: home.connectors_dir(),
        drafts: home.drafts_dir(),
    };
    let connectors = Arc::new(tern_lua::Connectors::open(lua.clone(), dirs.active.clone()));
    registry.add_source(connectors.clone());
    // Routines' watches are checked against the active connectors (T1b).
    let checks: Arc<dyn tern_tools::schedule::RoutineChecks> = Arc::new(
        crate::routines::LuaChecks::new(lua.clone(), dirs.active.clone()),
    );
    // Clients hear when a tool changes the routines.
    registry.register(Arc::new(tern_tools::schedule::ScheduleCreate {
        store: store.clone(),
        notify: Some(schedules.clone()),
        checks: Some(checks.clone()),
    }));
    registry.register(Arc::new(tern_tools::schedule::ScheduleUpdate {
        store: store.clone(),
        notify: Some(schedules.clone()),
        checks: Some(checks),
    }));
    registry.register(Arc::new(tern_tools::schedule::ScheduleDelete {
        store: store.clone(),
        notify: Some(schedules),
    }));
    registry.register(Arc::new(tern_lua::tools::ConnectorWrite {
        host: lua.clone(),
        dirs: dirs.clone(),
    }));
    registry.register(Arc::new(tern_lua::tools::ConnectorTest {
        host: lua.clone(),
        dirs: dirs.clone(),
        inspected: Default::default(),
    }));
    let mut from_openapi = tern_lua::tools::ConnectorFromOpenapi::new(lua.clone(), dirs.clone());
    from_openapi.default_tools = config.connectors.openapi_tools.max(1);
    from_openapi.max_tools = config.connectors.openapi_max_tools.max(1);
    registry.register(Arc::new(from_openapi));
    registry.register(Arc::new(tern_lua::tools::ConnectorActivate {
        host: lua,
        dirs,
        inspected: Default::default(),
    }));

    let mcp = Arc::new(
        tern_mcp::McpServers::from_config(
            &config.mcp.servers,
            Arc::new(secrets.clone()),
            Some(mcp_notify),
        )
        .with_connections(
            connections.clone(),
            Duration::from_secs(config.auth.idle_timeout_secs),
        ),
    );
    registry.add_source(mcp.clone());
    Ok((registry, connectors, mcp))
}
