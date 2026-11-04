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
use tern_core::types::{Provider, ProviderError};
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
    pub fn build(&self, role: AgentRole<'_>, depth: u32) -> String {
        let skills: Vec<(String, String)> = tern_core::skills::discover(&self.home.skills_dir())
            .into_iter()
            .map(|s| (s.name, s.description))
            .collect();
        let integrations = self
            .tools
            .upgrade()
            .map(|t| t.describe_sources())
            .unwrap_or_default();
        tern_core::prompt::system_prompt(&tern_core::prompt::PromptInputs {
            home: &self.home,
            role,
            // Only runs that get the agent_* tools hear about them.
            subagents: self.sub.as_ref().filter(|_| depth < self.max_depth),
            now: chrono::Local::now(),
            workspace: &self.workspace,
            skills: &skills,
            integrations: &integrations,
            memory: &self.memory,
        })
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
    /// The live MCP server set; config writes go through [`Runtime::reload_mcp`].
    pub(crate) mcp: Arc<tern_mcp::McpServers>,
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
        tools.register(Arc::new(tern_tools::secret_request::SecretRequest::new(
            approvals.clone(),
            secrets.clone(),
            store.clone(),
            secret_uses(home, live.clone(), &secrets, &store)?,
            Duration::from_secs(config.secret_request_timeout_secs),
        )));
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
            host.set_prompt(Box::new(move |label, depth| {
                p.build(AgentRole::Subagent { label }, depth)
            }));
            slot.set(host.clone());
            host
        });
        if let Some(h) = &subagents
            && let Err(e) = h.reconcile()
        {
            tracing::warn!(error = %e, "reconciling sub-agent reports failed");
        }
        Ok(Self {
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
            mcp,
            mock,
        })
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

    pub(crate) fn set_live(&self, config: Config) {
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
        if let Some(m) = flag {
            return Ok(self.resolve_model(m)?.model.parse()?);
        }
        if let Some(m) = session_model {
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
        let model = self.pick_model(model, session.model.as_deref())?;
        if session.model.as_deref() != Some(&model.to_string()) {
            self.store
                .set_session_model(&session.id, &model.to_string())?;
        }
        let role = if trigger == "cron" {
            AgentRole::Scheduled
        } else {
            AgentRole::Main
        };
        let system = self.prompts.build(role, 0);
        let run = self
            .agent
            .start_run(&session.id, text, &model, &system, trigger)?;
        Ok(StartedRun {
            session_id: session.id,
            run_id: run.id,
            model,
        })
    }
}

pub struct StartedRun {
    pub session_id: String,
    pub run_id: String,
    pub model: ModelRef,
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
        return Ok((Arc::new(move |_| Ok(mock.clone())), true));
    }
    let secrets = secrets.clone();
    let factory: ProviderFactory = Arc::new(move |name: &str| {
        let cfg = live
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .providers
            .get(name)
            .cloned();
        let cfg = cfg.ok_or_else(|| {
            ProviderError::Config(format!(
                "provider {name:?} is not configured in config.toml"
            ))
        })?;
        tern_providers::build(name, &cfg, &secrets)
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
    registry.register(Arc::new(tern_tools::memory::ProfileEdit::new(
        home.clone(),
        config.memory.clone(),
    )));
    if config.subagents.enabled && config.subagents.max_depth >= 1 {
        tern_tools::agents::register(&mut registry, subagents.clone());
    }

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
