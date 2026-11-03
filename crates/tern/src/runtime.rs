//! Builds the agent from config: store, providers, tools, journal.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use tern_core::agent::{Agent, AgentConfig, ProviderFactory};
use tern_core::approval::ApprovalHandler;
use tern_core::config::{ModelRef, SandboxBackendKind};
use tern_core::journal::Journal;
use tern_core::tool::ToolRegistry;
use tern_core::types::{Provider, ProviderError};
use tern_core::{Config, Secrets, TernHome};
use tern_providers::{MockProvider, PROVIDER_ENV};
use tern_store::Store;

pub struct Runtime {
    pub home: TernHome,
    pub config: Config,
    pub store: Store,
    pub agent: Agent,
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

        let (providers, mock) = provider_factory(&config, &secrets)?;
        let (tools, connectors) = build_tools(home, &config, &store, &secrets)?;
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
            journal,
            providers,
            tools,
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
                ..AgentConfig::default()
            },
        );
        Ok(Self {
            home: home.clone(),
            config,
            store,
            agent,
            mock,
        })
    }

    /// `--model`, else the session's model, else `default_model`.
    pub fn pick_model(
        &self,
        flag: Option<&str>,
        session_model: Option<&str>,
    ) -> anyhow::Result<ModelRef> {
        if let Some(m) = flag.or(session_model) {
            return Ok(m.parse()?);
        }
        match self.config.default_model_ref() {
            Some(m) => Ok(m?),
            None if self.mock => Ok("mock:mock".parse()?),
            None => bail!(
                "no model selected: pass --model provider:model or run `tern config set default_model provider:model`"
            ),
        }
    }

    /// Check that `model` parses and names a configured provider.
    pub fn check_model(&self, model: &str) -> anyhow::Result<ModelRef> {
        let m: ModelRef = model.parse()?;
        if !self.mock && !self.config.providers.contains_key(&m.provider) {
            bail!("provider {:?} is not configured in config.toml", m.provider);
        }
        Ok(m)
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
        let skills: Vec<(String, String)> = tern_core::skills::discover(&self.home.skills_dir())
            .into_iter()
            .map(|s| (s.name, s.description))
            .collect();
        let integrations = self.agent.tools().describe_sources();
        let system = tern_core::prompt::system_prompt(&tern_core::prompt::PromptInputs {
            home: &self.home,
            now: chrono::Local::now(),
            workspace: &self.config.workspace_dir(),
            skills: &skills,
            integrations: &integrations,
        });
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

fn provider_factory(config: &Config, secrets: &Secrets) -> anyhow::Result<(ProviderFactory, bool)> {
    if let Some(spec) = std::env::var(PROVIDER_ENV).ok().filter(|v| !v.is_empty()) {
        let Some(path) = spec.strip_prefix("mock:") else {
            bail!("{PROVIDER_ENV} must be `mock:<script.json>`, got {spec:?}");
        };
        let mock: Arc<dyn Provider> = Arc::new(MockProvider::from_file(path.as_ref())?);
        return Ok((Arc::new(move |_| Ok(mock.clone())), true));
    }
    let providers = config.providers.clone();
    let secrets = secrets.clone();
    let factory: ProviderFactory = Arc::new(move |name: &str| {
        let cfg = providers.get(name).ok_or_else(|| {
            ProviderError::Config(format!(
                "provider {name:?} is not configured in config.toml"
            ))
        })?;
        tern_providers::build(name, cfg, &secrets)
    });
    Ok((factory, false))
}

fn build_tools(
    home: &TernHome,
    config: &Config,
    store: &Store,
    secrets: &Secrets,
) -> anyhow::Result<(ToolRegistry, Arc<tern_lua::Connectors>)> {
    let sandbox: Arc<dyn tern_sandbox::SandboxBackend> = match config.sandbox.backend {
        SandboxBackendKind::Host => Arc::new(tern_sandbox::HostBackend),
        SandboxBackendKind::Docker => Arc::new(tern_sandbox::DockerBackend::new(
            "docker",
            &config.sandbox.docker_image,
            &config.workspace_dir(),
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

    registry.add_source(Arc::new(tern_mcp::McpServers::from_config(
        &config.mcp.servers,
        Arc::new(secrets.clone()),
    )));
    Ok((registry, connectors))
}
