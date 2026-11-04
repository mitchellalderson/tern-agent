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
        let mcp_changes = changes.clone();
        let notify: tern_mcp::Notify = Arc::new(move |name, state| {
            let _ = mcp_changes.send(Change::McpChanged {
                name: name.to_owned(),
                state,
            });
        });
        let (mut tools, connectors, mcp) =
            build_tools(home, &config, &store, &secrets, &slot, notify)?;
        // Channels (docs/channels-design.md §3): a post an agent writes is
        // announced like the daemon's own.
        let post_changes = changes.clone();
        tools.register(Arc::new(tern_tools::channels::ChannelRead::new(
            store.clone(),
        )));
        tools.register(Arc::new(tern_tools::channels::ChannelPost::new(
            store.clone(),
            Some(Arc::new(move |row| {
                let _ = post_changes.send(Change::PostCreated {
                    post: Box::new(tern_core::channels::Post::from(row)),
                });
            })),
        )));
        tools.register(Arc::new(tern_tools::secret_request::SecretRequest::new(
            approvals.clone(),
            secrets.clone(),
            store.clone(),
            secret_uses(home, live.clone(), &secrets, &store)?,
            Duration::from_secs(config.secret_request_timeout_secs),
        )));
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
            host.set_prompt(Box::new(move |label, depth, parent_session| {
                // A helper works under its parent agent's persona and memory.
                let frame = st
                    .session_agent(parent_session)
                    .ok()
                    .and_then(|a| st.agent(&a).ok().flatten())
                    .map(|a| agent_frame(&h, &st, &a));
                let (prompt, scope) = match &frame {
                    Some((prompt, scope)) => (prompt.as_ref(), scope.as_ref()),
                    None => (None, None),
                };
                p.build(AgentRole::Subagent { label }, depth, prompt, scope)
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
        let id = agents::id_for(&slug);
        if self.store.agent(&id)?.is_some() {
            bail!("an archived agent already has the id {id}; unarchive it or pick another name");
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

    /// Check that `model` resolves to a configured provider.
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
        let agent = self
            .store
            .agent(session.agent())?
            .with_context(|| format!("agent {} not found", session.agent()))?;
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
        if let Some(c) = self.store.session_channel(&session.id)?
            && let Some(section) = c
                .instructions
                .as_deref()
                .and_then(|i| tern_core::channels::instructions_section(&c.slug, i))
        {
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
        )?;
        // Frozen on the run, like its model (tooling spec §5.4); a routine
        // overrides it in `Manager::start_scheduled`. An agent's autonomy
        // ceiling (agent profiles AP3) will cap this value here.
        self.store
            .set_run_autonomy(&run.id, self.live().autonomy.as_str())?;
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
    let prompt = tern_core::prompt::AgentPrompt {
        persona_path: tern_core::agents::persona_path(home, &agent.id),
        memory_path: tern_core::agents::memory_path(home, &agent.id, agent.private_memory()),
        others,
    };
    (Some(prompt), scope)
}

/// One line per other active agent: `@slug · Name · first line of its persona`.
fn other_agents(home: &TernHome, store: &Store, me: &str) -> Vec<String> {
    store
        .agents(false)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.id != me)
        .map(|a| {
            let first = std::fs::read_to_string(tern_core::agents::persona_path(home, &a.id))
                .ok()
                .and_then(|t| {
                    t.lines()
                        .map(str::trim)
                        .find(|l| !l.is_empty() && !l.starts_with("<!--"))
                        .map(|l| l.chars().take(120).collect::<String>())
                })
                .unwrap_or_default();
            format!("@{} · {} · {first}", a.slug, a.name)
        })
        .collect()
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

fn build_tools(
    home: &TernHome,
    config: &Config,
    store: &Store,
    secrets: &Secrets,
    subagents: &HostSlot,
    mcp_notify: tern_mcp::Notify,
) -> anyhow::Result<(
    ToolRegistry,
    Arc<tern_lua::Connectors>,
    Arc<tern_mcp::McpServers>,
)> {
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
    registry.register(Arc::new(tern_python::Python::default()));

    let lua = tern_lua::Host::new(store.clone(), secrets.clone())?;
    let dirs = tern_lua::tools::Dirs {
        active: home.connectors_dir(),
        drafts: home.drafts_dir(),
    };
    let connectors = Arc::new(tern_lua::Connectors::open(lua.clone(), dirs.active.clone()));
    registry.add_source(connectors.clone());
    registry.register(Arc::new(tern_lua::tools::ConnectorWrite {
        host: lua.clone(),
        dirs: dirs.clone(),
    }));
    registry.register(Arc::new(tern_lua::tools::ConnectorTest {
        host: lua.clone(),
        dirs: dirs.clone(),
        inspected: Default::default(),
    }));
    registry.register(Arc::new(tern_lua::tools::ConnectorActivate {
        host: lua,
        dirs,
        inspected: Default::default(),
    }));

    let mcp = Arc::new(tern_mcp::McpServers::from_config(
        &config.mcp.servers,
        Arc::new(secrets.clone()),
        Some(mcp_notify),
    ));
    registry.add_source(mcp.clone());
    Ok((registry, connectors, mcp))
}
