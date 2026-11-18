//! Downloads the daemon runs for the app and the CLI: one at a time, owned
//! by the daemon (so they go on when a window closes), cancellable, and
//! checked against free disk space. Progress goes out as `local_pull`
//! events; the list is `GET /v1/local/pulls`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tern_core::models::Change;
use tokio_util::sync::CancellationToken;

use super::{Error, Local, Progress};

/// Free space a download must leave: models are big, and a full disk hurts
/// the whole Mac.
pub const DISK_MARGIN: u64 = 10 * (1 << 30);
/// Finished downloads kept for the list.
const KEEP_FINISHED: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Starting,
    Downloading,
    /// Checking the files and writing the manifest.
    Verifying,
    Done,
    Failed,
    Cancelled,
}

impl Phase {
    pub fn finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

/// A download.
#[derive(Debug, Clone, Serialize)]
pub struct Pull {
    pub id: String,
    pub model: String,
    pub phase: Phase,
    /// Bytes done and expected across all layers seen so far.
    pub completed: u64,
    pub total: u64,
    pub error: Option<String>,
}

#[derive(Default)]
pub(super) struct Pulls {
    jobs: Mutex<Vec<(Pull, CancellationToken)>>,
}

impl Pulls {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(Pull, CancellationToken)>> {
        self.jobs.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Pull)) -> Option<Pull> {
        let mut jobs = self.lock();
        let (p, _) = jobs.iter_mut().find(|(p, _)| p.id == id)?;
        f(p);
        Some(p.clone())
    }

    pub(super) fn list(&self) -> Vec<Pull> {
        self.lock().iter().map(|(p, _)| p.clone()).collect()
    }
}

/// A model name Ollama takes: `qwen3:8b`, `library/x`, `hf.co/user/model:Q4`.
pub fn valid_model_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with(['-', '.', '/', ':'])
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':' | b'/'))
}

fn phase_of(status: &str) -> Phase {
    if status.starts_with("pulling manifest") {
        Phase::Starting
    } else if status.starts_with("pulling") || status.starts_with("downloading") {
        Phase::Downloading
    } else {
        // `verifying sha256 digest`, `writing manifest`, `removing any unused layers`.
        Phase::Verifying
    }
}

fn gib(n: u64) -> String {
    format!("{:.1} GB", n as f64 / (1u64 << 30) as f64)
}

impl Local {
    pub fn pulls(&self) -> Vec<Pull> {
        self.pulls.list()
    }

    fn announce(&self, p: &Pull) {
        let _ = self.changes.send(Change::LocalPull {
            id: p.id.clone(),
            model: p.model.clone(),
            phase: serde_json::to_value(p.phase)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            completed: p.completed,
            total: p.total,
            error: p.error.clone(),
        });
    }

    /// Start downloading `model`. Needs the engine running and room on the
    /// disk. One download at a time.
    pub async fn pull(self: &Arc<Self>, model: &str) -> Result<Pull, Error> {
        let model = model.trim();
        if !valid_model_name(model) {
            return Err(Error::Failed(format!("{model:?} isn't a model name")));
        }
        if let Some(active) = self
            .pulls
            .lock()
            .iter()
            .find(|(p, _)| !p.phase.finished())
            .map(|(p, _)| p.clone())
        {
            return if active.model == model {
                Ok(active)
            } else {
                Err(Error::Unavailable(format!(
                    "{} is still downloading. Wait for it, or cancel it.",
                    active.model
                )))
            };
        }
        let status = self.engine.status().await;
        if !status.detection.running {
            return Err(Error::Unavailable(
                "Ollama isn't running. Start it first.".into(),
            ));
        }
        if let Some(free) = status.disk_free.filter(|f| *f < DISK_MARGIN) {
            return Err(Error::Failed(format!(
                "Only {} free. tern keeps at least {} free before downloading a model.",
                gib(free),
                gib(DISK_MARGIN)
            )));
        }
        let job = Pull {
            id: uuid::Uuid::new_v4().simple().to_string()[..8].to_owned(),
            model: model.to_owned(),
            phase: Phase::Starting,
            completed: 0,
            total: 0,
            error: None,
        };
        let cancel = CancellationToken::new();
        {
            let mut jobs = self.pulls.lock();
            jobs.push((job.clone(), cancel.clone()));
            // Forget the oldest finished ones.
            let finished = jobs.iter().filter(|(p, _)| p.phase.finished()).count();
            for _ in 0..finished.saturating_sub(KEEP_FINISHED) {
                if let Some(i) = jobs.iter().position(|(p, _)| p.phase.finished()) {
                    jobs.remove(i);
                }
            }
        }
        self.announce(&job);
        let me = self.clone();
        let free = status.disk_free;
        let id = job.id.clone();
        let name = model.to_owned();
        tokio::spawn(async move {
            me.run_pull(&id, &name, free, cancel).await;
        });
        Ok(job)
    }

    async fn run_pull(&self, id: &str, model: &str, free: Option<u64>, cancel: CancellationToken) {
        // Layers arrive one after another; add them up for one bar.
        let layers = Mutex::new(BTreeMap::<String, (u64, u64)>::new());
        let last = Mutex::new(Instant::now());
        let too_big = Mutex::new(None::<String>);
        let stop = cancel.clone();
        let result = self
            .engine
            .pull(
                model,
                &mut |p: Progress| {
                    let phase = phase_of(&p.status);
                    let (completed, total) = {
                        let mut l = layers.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(d) = &p.digest
                            && p.total > 0
                        {
                            l.insert(d.clone(), (p.completed, p.total));
                        }
                        l.values().fold((0, 0), |(c, t), (lc, lt)| (c + lc, t + lt))
                    };
                    // Not enough room for what the layers add up to: stop now,
                    // not when the disk is full.
                    if let Some(free) = free
                        && total > free.saturating_sub(DISK_MARGIN)
                    {
                        *too_big.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!(
                            "{model} needs about {}, and only {} is free (tern keeps {} free).",
                            gib(total),
                            gib(free),
                            gib(DISK_MARGIN)
                        ));
                        stop.cancel();
                        return;
                    }
                    let mut at = last.lock().unwrap_or_else(|e| e.into_inner());
                    let changed = self
                        .pulls
                        .list()
                        .iter()
                        .find(|j| j.id == id)
                        .is_none_or(|j| j.phase != phase);
                    // Many updates a second would flood the event stream.
                    if changed || at.elapsed() >= Duration::from_millis(250) {
                        *at = Instant::now();
                        if let Some(j) = self.pulls.update(id, |j| {
                            j.phase = phase;
                            j.completed = completed;
                            j.total = total;
                        }) {
                            self.announce(&j);
                        }
                    }
                },
                cancel,
            )
            .await;
        let big = too_big.lock().unwrap_or_else(|e| e.into_inner()).take();
        let (phase, error) = match result {
            Ok(()) => (Phase::Done, None),
            Err(_) if big.is_some() => (Phase::Failed, big),
            Err(Error::Cancelled) => (Phase::Cancelled, None),
            Err(e) => (Phase::Failed, Some(e.to_string())),
        };
        if let Some(j) = self.pulls.update(id, |j| {
            j.phase = phase;
            j.error = error;
            if phase == Phase::Done {
                j.completed = j.total;
            }
        }) {
            self.announce(&j);
        }
        let _ = self.changes.send(Change::LocalChanged {});
    }

    /// Stop a download. A later pull of the same model resumes it.
    pub fn cancel_pull(&self, id: &str) -> Result<(), Error> {
        let jobs = self.pulls.lock();
        match jobs.iter().find(|(p, _)| p.id == id) {
            Some((p, c)) if !p.phase.finished() => {
                c.cancel();
                Ok(())
            }
            Some(_) => Err(Error::Unavailable("that download already ended".into())),
            None => Err(Error::Failed("no such download".into())),
        }
    }

    /// Remove an installed model.
    pub async fn delete(&self, model: &str) -> Result<(), Error> {
        if !valid_model_name(model) {
            return Err(Error::Failed(format!("{model:?} isn't a model name")));
        }
        let r = self.engine.delete(model).await;
        let _ = self.changes.send(Change::LocalChanged {});
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local::install::Installer;
    use crate::local::{Detection, Engine, LocalModel, Status};
    use async_trait::async_trait;

    #[test]
    fn model_names() {
        for ok in [
            "qwen3:8b",
            "llama3.2",
            "library/x:latest",
            "hf.co/u/m:Q4_K_M",
        ] {
            assert!(valid_model_name(ok), "{ok}");
        }
        for bad in [
            "",
            "-rf",
            "../x",
            "a b",
            "a;b",
            "a\nb",
            ".hidden",
            "/abs",
            &"x".repeat(201),
        ] {
            assert!(!valid_model_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn phases() {
        assert_eq!(phase_of("pulling manifest"), Phase::Starting);
        assert_eq!(phase_of("pulling 2af3b81862c6"), Phase::Downloading);
        assert_eq!(phase_of("verifying sha256 digest"), Phase::Verifying);
        assert_eq!(phase_of("writing manifest"), Phase::Verifying);
    }

    /// An engine that plays back a script of progress and never touches
    /// the network.
    struct Scripted {
        running: bool,
        installed: bool,
        /// Times `start` ran.
        starts: std::sync::atomic::AtomicUsize,
        free: Option<u64>,
        steps: Vec<Progress>,
        /// Wait for the token after the steps instead of finishing.
        hang: bool,
        deleted: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Engine for Scripted {
        fn id(&self) -> &'static str {
            "scripted"
        }
        async fn detect(&self) -> Detection {
            Detection {
                installed: self.installed,
                running: self.running,
                ..Detection::default()
            }
        }
        async fn status(&self) -> Status {
            Status {
                engine: "",
                detection: self.detect().await,
                managed: false,
                url: String::new(),
                models_dir: None,
                disk_free: self.free,
                ram_bytes: None,
                context: None,
                context_source: None,
                warnings: vec![],
                install_page: "",
                install: crate::local::install::Plan {
                    source: String::new(),
                    destination: String::new(),
                    summary: String::new(),
                    available: false,
                },
                installing: crate::local::install::State::Idle,
                starter: None,
            }
        }
        async fn list(&self) -> Result<Vec<LocalModel>, Error> {
            Ok(vec![])
        }
        async fn pull(
            &self,
            _model: &str,
            progress: &mut (dyn FnMut(Progress) + Send),
            cancel: CancellationToken,
        ) -> Result<(), Error> {
            for s in &self.steps {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                progress(s.clone());
            }
            if self.hang {
                cancel.cancelled().await;
                return Err(Error::Cancelled);
            }
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            Ok(())
        }
        async fn delete(&self, model: &str) -> Result<(), Error> {
            self.deleted.lock().unwrap().push(model.to_owned());
            Ok(())
        }
        async fn start(&self) -> Result<(), Error> {
            self.starts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        async fn stop(&self) -> Result<(), Error> {
            Ok(())
        }
    }

    const GB: u64 = 1 << 30;

    fn step(status: &str, digest: Option<&str>, completed: u64, total: u64) -> Progress {
        Progress {
            status: status.into(),
            digest: digest.map(str::to_owned),
            completed,
            total,
        }
    }

    fn local(engine: Scripted) -> (Arc<Local>, tokio::sync::broadcast::Receiver<Change>) {
        let (tx, rx) = tokio::sync::broadcast::channel(256);
        let l = Local::new(Arc::new(engine), Installer::system(), tx);
        (Arc::new(l), rx)
    }

    fn engine() -> Scripted {
        Scripted {
            running: true,
            installed: true,
            starts: Default::default(),
            free: Some(100 * GB),
            steps: vec![
                step("pulling manifest", None, 0, 0),
                step("pulling aaa", Some("aaa"), GB, 4 * GB),
                step("pulling aaa", Some("aaa"), 4 * GB, 4 * GB),
                step("pulling bbb", Some("bbb"), 0, 1000),
                step("verifying sha256 digest", None, 0, 0),
                step("success", None, 0, 0),
            ],
            hang: false,
            deleted: Mutex::new(vec![]),
        }
    }

    async fn finished(l: &Arc<Local>, id: &str) -> Pull {
        for _ in 0..200 {
            if let Some(p) = l.pulls().into_iter().find(|p| p.id == id)
                && p.phase.finished()
            {
                return p;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("download never finished");
    }

    #[tokio::test]
    async fn a_download_runs_to_done_and_announces_it() {
        let (l, mut rx) = local(engine());
        let job = l.pull("qwen3:8b").await.unwrap();
        let p = finished(&l, &job.id).await;
        assert_eq!(p.phase, Phase::Done, "{p:?}");
        assert_eq!(p.completed, p.total);
        assert_eq!(p.total, 4 * GB + 1000);
        let mut phases = Vec::new();
        while let Ok(c) = rx.try_recv() {
            if let Change::LocalPull { phase, model, .. } = c {
                assert_eq!(model, "qwen3:8b");
                if phases.last() != Some(&phase) {
                    phases.push(phase);
                }
            }
        }
        assert_eq!(phases.first().map(String::as_str), Some("starting"));
        assert_eq!(phases.last().map(String::as_str), Some("done"));
    }

    #[tokio::test]
    async fn refuses_when_ollama_is_off_the_disk_is_tight_or_the_name_is_bad() {
        let mut e = engine();
        e.running = false;
        let (l, _rx) = local(e);
        assert!(matches!(l.pull("x").await, Err(Error::Unavailable(m)) if m.contains("Start it")));

        let mut e = engine();
        e.free = Some(5 * GB);
        let (l, _rx) = local(e);
        let err = l.pull("x").await.unwrap_err().to_string();
        assert!(err.contains("5.0 GB free"), "{err}");

        let (l, _rx) = local(engine());
        assert!(l.pull("a b").await.is_err());
    }

    #[tokio::test]
    async fn stops_when_the_layers_will_not_fit() {
        let mut e = engine();
        // 4 GB model, 12 GB free, 10 GB margin: 2 GB of room.
        e.free = Some(12 * GB);
        let (l, _rx) = local(e);
        let job = l.pull("big").await.unwrap();
        let p = finished(&l, &job.id).await;
        assert_eq!(p.phase, Phase::Failed);
        let msg = p.error.unwrap();
        assert!(
            msg.contains("needs about 4.0 GB") && msg.contains("12.0 GB"),
            "{msg}"
        );
    }

    #[tokio::test]
    async fn one_at_a_time_and_cancel() {
        let mut e = engine();
        e.hang = true;
        e.steps.truncate(2);
        let (l, _rx) = local(e);
        let a = l.pull("a").await.unwrap();
        // The same model again is the same download; another is refused.
        assert_eq!(l.pull("a").await.unwrap().id, a.id);
        assert!(
            matches!(l.pull("b").await, Err(Error::Unavailable(m)) if m.contains("still downloading"))
        );
        l.cancel_pull(&a.id).unwrap();
        let p = finished(&l, &a.id).await;
        assert_eq!(p.phase, Phase::Cancelled);
        assert!(l.cancel_pull(&a.id).is_err());
        assert!(l.cancel_pull("nope").is_err());
        // Free again.
        assert!(l.pull("b").await.is_ok());
    }

    #[tokio::test]
    async fn delete_checks_the_name_and_calls_the_engine() {
        let (l, mut rx) = local(engine());
        l.delete("qwen3:8b").await.unwrap();
        assert!(l.delete("../etc").await.is_err());
        assert!(matches!(rx.try_recv(), Ok(Change::LocalChanged {})));
    }

    #[tokio::test]
    async fn ensure_running_reuses_starts_or_says_not_installed() {
        // Running: nothing to do.
        let (l, _rx) = local(engine());
        l.ensure_running().await.unwrap();
        // Installed, not running: started once, no prompt.
        let mut e = engine();
        e.running = false;
        let (l, _rx) = local(e);
        l.ensure_running().await.unwrap();
        // Not installed: the one thing a person must do.
        let mut e = engine();
        e.running = false;
        e.installed = false;
        let (l, _rx) = local(e);
        assert!(matches!(
            l.ensure_running().await,
            Err(Error::NotInstalled(_))
        ));
    }
}
