//! First run with Ollama in the terminal: "Set up with Ollama" installs it
//! if needed, tern starts it by itself, a starter model sized to the Mac is
//! downloaded, and it becomes the default (docs/local-models.md). One
//! [`poll`] is one step of that, run as a background task; the panel
//! schedules the next while the step is open.

use std::time::Duration;

use tern_core::models::{LocalInfo, LocalModelInfo, LocalPullInfo};

use crate::{Backend, BackendResult};

/// How long a poll waits before it looks again.
const EVERY: Duration = Duration::from_millis(1200);

/// What one poll may do besides look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollOpts {
    /// The first poll after opening: no wait.
    pub first: bool,
    /// The person agreed: download the starter when there is no model.
    pub go: bool,
    /// Download and install Ollama now (once, after `go`).
    pub install: bool,
    /// Start Ollama if it is installed and off.
    pub try_start: bool,
}

/// What a poll found.
#[derive(Debug, Clone, Default)]
pub struct LocalView {
    pub info: LocalInfo,
    pub pulls: Vec<LocalPullInfo>,
    pub models: Vec<LocalModelInfo>,
    /// Starting Ollama failed: say why, offer "try again".
    pub start_error: Option<String>,
    /// Another step failed (install, download).
    pub error: Option<String>,
}

impl LocalView {
    /// A model that can run tasks.
    pub fn usable_model(&self) -> Option<&LocalModelInfo> {
        self.models.iter().find(|m| m.tools != Some(false))
    }

    pub fn active_pull(&self) -> Option<&LocalPullInfo> {
        self.pulls.iter().find(|p| !p.finished())
    }
}

pub async fn poll(backend: &dyn Backend, o: PollOpts) -> BackendResult<LocalView> {
    if !o.first {
        tokio::time::sleep(EVERY).await;
    }
    let mut view = LocalView::default();
    if o.install
        && let Err(e) = backend.local_install().await
    {
        view.error = Some(e);
    }
    let mut info = backend.local_status().await?;
    if info.installed && !info.running && o.try_start {
        match backend.local_start().await {
            Ok(()) => info = backend.local_status().await?,
            Err(e) => view.start_error = Some(e),
        }
    }
    if info.running {
        view.pulls = backend.local_pulls().await.unwrap_or_default();
        view.models = backend.local_models().await.unwrap_or_default();
        let idle = view.active_pull().is_none();
        if o.go
            && !info.too_old
            && idle
            && view.usable_model().is_none()
            && let Some(s) = &info.starter
            && let Err(e) = backend.local_pull(&s.model).await
        {
            view.error = Some(e);
        }
        // A failed download is worth saying.
        if view.error.is_none()
            && let Some(p) = view.pulls.last().filter(|p| p.phase == "failed")
        {
            view.error = p.error.clone();
        }
    }
    view.info = info;
    Ok(view)
}
