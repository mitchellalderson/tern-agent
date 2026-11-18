//! Models that run on this computer (docs/local-models.md).
//!
//! tern drives an outside engine over HTTP. [`Engine`] is the small
//! surface it needs from one: detect, status, list, pull, delete, start and
//! stop. Ollama is the first ([`ollama::Ollama`]); MLX or `llama-server`
//! would be another implementation, not a change to the daemon.
//!
//! [`Local`] is what the daemon holds: the engine, plus the "install it for
//! me" job and its state.

pub mod catalog;
pub mod install;
pub mod ollama;
pub mod pulls;
pub mod starters;
pub mod tiers;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Serialize;
use tern_core::models::Change;
use tokio::sync::broadcast;

/// Where Ollama listens unless told otherwise: this computer only.
pub const OLLAMA_URL: &str = "http://127.0.0.1:11434";
/// Where a person gets the engine themselves.
pub const INSTALL_PAGE: &str = "https://ollama.com/download";

/// What is on this computer, without asking the engine anything it
/// doesn't have to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Detection {
    /// The app or the command is there (it may not be running).
    pub installed: bool,
    /// The server answers on its address. A server someone else started
    /// counts: tern always reuses it.
    pub running: bool,
    pub version: Option<String>,
    /// Older than tern can use ([`tern_providers::ollama::MIN_VERSION`]).
    pub too_old: bool,
}

/// Where the context in [`Status::context`] comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    /// A loaded model reports it.
    Loaded,
    /// tern started the server with it.
    Managed,
    /// A guess from this computer's memory: nothing is loaded yet.
    Estimate,
}

/// `GET /v1/local/status`.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// Which engine (`ollama`).
    pub engine: &'static str,
    #[serde(flatten)]
    pub detection: Detection,
    /// tern started this server and stops it with the daemon. A server the
    /// user runs is used as is: tern never changes its settings.
    pub managed: bool,
    pub url: String,
    pub models_dir: Option<String>,
    pub disk_free: Option<u64>,
    pub ram_bytes: Option<u64>,
    /// Tokens of context models get, the smallest in use.
    pub context: Option<u64>,
    pub context_source: Option<ContextSource>,
    /// Plain-words advice: too old, context too small.
    pub warnings: Vec<String>,
    /// Where to get it by hand.
    pub install_page: &'static str,
    /// "Install for me": what it would do, and how far it has got.
    pub install: install::Plan,
    pub installing: install::State,
    /// The model first run offers, sized to this Mac. Filled by [`Local`].
    pub starter: Option<starters::Starter>,
}

/// One installed model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LocalModel {
    pub name: String,
    pub size: u64,
    pub digest: String,
    pub family: Option<String>,
    pub parameter_size: Option<String>,
    pub quantization: Option<String>,
    /// Can call tools. `None`: the server didn't say.
    pub tools: Option<bool>,
    pub thinking: Option<bool>,
    /// The model's own context limit.
    pub max_context: Option<u64>,
    /// In memory now.
    pub loaded: bool,
}

/// One step of a download, as the engine reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// `pulling manifest`, `pulling <digest>`, `verifying sha256 digest`, ...
    pub status: String,
    /// The layer this step is about, when it is one.
    pub digest: Option<String>,
    pub completed: u64,
    pub total: u64,
}

#[derive(Debug)]
pub enum Error {
    NotInstalled(&'static str),
    Cancelled,
    /// Not possible right now (nothing to stop, another program on the port).
    Unavailable(String),
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled(name) => write!(f, "{name} isn't installed"),
            Self::Unavailable(m) | Self::Failed(m) => f.write_str(m),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for Error {}

/// A model engine tern can drive.
#[async_trait]
pub trait Engine: Send + Sync {
    fn id(&self) -> &'static str;
    /// Installed? Running? Which version?
    async fn detect(&self) -> Detection;
    async fn status(&self) -> Status;
    /// Installed models, with what each can do.
    async fn list(&self) -> Result<Vec<LocalModel>, Error>;
    /// Download `model`, reporting progress. The token stops it; a later
    /// pull resumes the part already there.
    async fn pull(
        &self,
        model: &str,
        progress: &mut (dyn FnMut(Progress) + Send),
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), Error>;
    async fn delete(&self, model: &str) -> Result<(), Error>;
    /// Start the server unless one already answers.
    async fn start(&self) -> Result<(), Error>;
    /// Stop a server tern started. One the user runs is left alone.
    async fn stop(&self) -> Result<(), Error>;
}

/// What the daemon holds for local models.
pub struct Local {
    engine: Arc<dyn Engine>,
    installer: Arc<install::Installer>,
    installing: Mutex<install::State>,
    pulls: pulls::Pulls,
    /// "Set up all three": running, and why it last stopped.
    preset: Mutex<(bool, Option<String>)>,
    changes: broadcast::Sender<Change>,
}

impl Local {
    pub fn new(
        engine: Arc<dyn Engine>,
        installer: install::Installer,
        changes: broadcast::Sender<Change>,
    ) -> Self {
        Self {
            engine,
            installer: Arc::new(installer),
            installing: Mutex::new(install::State::Idle),
            pulls: pulls::Pulls::default(),
            preset: Mutex::new((false, None)),
            changes,
        }
    }

    /// The real engine and installer. Reads `TERN_LOCAL_URL` and
    /// `TERN_LOCAL_APPS` (a `:`-separated list of folders to look for the
    /// app in) so tests never see this computer's Ollama.
    pub fn system(changes: broadcast::Sender<Change>) -> Self {
        let engine = ollama::Ollama::new(ollama::Config::system());
        Self::new(Arc::new(engine), install::Installer::system(), changes)
    }

    fn state(&self) -> install::State {
        self.installing
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn set_state(&self, s: install::State) {
        *self.installing.lock().unwrap_or_else(|p| p.into_inner()) = s;
        let _ = self.changes.send(Change::LocalChanged {});
    }

    pub async fn status(&self) -> Status {
        let mut s = self.engine.status().await;
        s.engine = self.engine.id();
        s.starter = Some(starters::starter_for(s.ram_bytes));
        s.install = self.installer.plan();
        s.installing = self.state();
        s
    }

    pub fn preset_state(&self) -> (bool, Option<String>) {
        self.preset
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn set_preset(&self, running: bool, error: Option<String>) {
        *self.preset.lock().unwrap_or_else(|p| p.into_inner()) = (running, error);
    }

    pub async fn models(&self) -> Result<Vec<LocalModel>, Error> {
        self.engine.list().await
    }

    /// Make sure Ollama answers: reuse the running one, or start it when it
    /// is installed. Never prompts. Called at daemon start, before a run on
    /// an Ollama model, and during first-run setup.
    pub async fn ensure_running(&self) -> Result<(), Error> {
        let d = self.engine.detect().await;
        if d.running {
            return Ok(());
        }
        if !d.installed {
            return Err(Error::NotInstalled("Ollama"));
        }
        self.start().await
    }

    pub async fn start(&self) -> Result<(), Error> {
        let r = self.engine.start().await;
        let _ = self.changes.send(Change::LocalChanged {});
        r
    }

    pub async fn stop(&self) -> Result<(), Error> {
        let r = self.engine.stop().await;
        let _ = self.changes.send(Change::LocalChanged {});
        r
    }

    /// Download, verify and install the engine's app, in the background.
    /// Only after the person said yes (`consent`).
    pub async fn install(self: &Arc<Self>, consent: bool) -> Result<(), Error> {
        if !consent {
            return Err(Error::Failed(
                "tern only downloads Ollama after you agree".into(),
            ));
        }
        if self.engine.detect().await.installed {
            return Err(Error::Failed("Ollama is already installed".into()));
        }
        {
            let mut st = self.installing.lock().unwrap_or_else(|p| p.into_inner());
            if st.busy() {
                return Err(Error::Failed("Ollama is already being installed".into()));
            }
            *st = install::State::Working {
                phase: install::Phase::Fetching,
                completed: 0,
                total: 0,
            };
        }
        let _ = self.changes.send(Change::LocalChanged {});
        let me = self.clone();
        tokio::spawn(async move {
            let last = Mutex::new((std::time::Instant::now(), install::Phase::Fetching));
            let result = me
                .installer
                .run(&|phase, completed, total| {
                    // Progress many times a second would flood the stream.
                    let mut l = last.lock().unwrap_or_else(|p| p.into_inner());
                    if phase != l.1 || l.0.elapsed() >= std::time::Duration::from_millis(250) {
                        *l = (std::time::Instant::now(), phase);
                        me.set_state(install::State::Working {
                            phase,
                            completed,
                            total,
                        });
                    }
                })
                .await;
            match result {
                Ok(done) => {
                    tracing::info!(app = %done.app.display(), signer = %done.signer, "Ollama installed");
                    me.set_state(install::State::Done {
                        signer: done.signer,
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "installing Ollama failed");
                    me.set_state(install::State::Failed {
                        message: e.to_string(),
                    });
                }
            }
        });
        Ok(())
    }
}
