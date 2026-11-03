//! Runs in the daemon: one active run per session, background driving,
//! delivery of results, and routing approvals to whoever can answer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tern_core::agent::{RunOutcome, RunStatus};
use tern_core::approval::{ApprovalHandler, ApprovalRequest, Decision};
use tern_core::events::AgentEvent;
use tern_gateway::{ChatHost, Gateway};
use tokio::sync::{broadcast, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::runtime::Runtime;

struct Active {
    run_id: String,
    cancel: CancellationToken,
    started: Instant,
    done: watch::Receiver<bool>,
}

/// An approval waiting for an API client (attached TUI) to answer.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ApiApproval {
    pub id: String,
    pub run_id: String,
    pub session_id: String,
    pub tool: String,
    pub input: serde_json::Value,
    pub summary: String,
    pub pattern: Option<String>,
}

/// Approvals for API clients: published on a channel, answered over HTTP.
#[derive(Default)]
pub struct ApiApprovals {
    pending: Mutex<HashMap<String, (ApiApproval, oneshot::Sender<Decision>)>>,
    tx: OnceLock<broadcast::Sender<ApiApproval>>,
}

impl ApiApprovals {
    fn tx(&self) -> &broadcast::Sender<ApiApproval> {
        self.tx.get_or_init(|| broadcast::channel(64).0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ApiApproval> {
        self.tx().subscribe()
    }

    /// Ask attached clients; deny at once if none is listening.
    async fn ask(&self, req: &ApprovalRequest) -> Decision {
        let (tx, rx) = oneshot::channel();
        let msg = ApiApproval {
            id: req.id.clone(),
            run_id: req.run_id.clone(),
            session_id: req.session_id.clone(),
            tool: req.call.name.clone(),
            input: req.call.input.clone(),
            summary: req.summary.clone(),
            pattern: req.pattern.clone(),
        };
        if let Ok(mut p) = self.pending.lock() {
            p.insert(req.id.clone(), (msg.clone(), tx));
        }
        if self.tx().send(msg).is_err() {
            if let Ok(mut p) = self.pending.lock() {
                p.remove(&req.id);
            }
            return Decision::Deny;
        }
        rx.await.unwrap_or(Decision::Deny)
    }

    pub fn answer(&self, id: &str, decision: Decision) -> bool {
        let tx = self.pending.lock().ok().and_then(|mut p| p.remove(id));
        tx.is_some_and(|(_, tx)| tx.send(decision).is_ok())
    }

    /// Approvals still waiting for an answer, for clients that attach late.
    pub fn pending(&self) -> Vec<ApiApproval> {
        self.pending
            .lock()
            .map(|p| p.values().map(|(a, _)| a.clone()).collect())
            .unwrap_or_default()
    }
}

/// Late-bound pieces the approval router needs (the runtime is built with
/// the router, so they can't be passed to its constructor).
#[derive(Default)]
pub struct Routes {
    pub store: OnceLock<tern_store::Store>,
    pub gateways: OnceLock<HashMap<String, Arc<dyn Gateway>>>,
    pub api: ApiApprovals,
}

/// Sends each approval to the chat the run reports to, else to API clients.
pub struct Router(pub Arc<Routes>);

#[async_trait]
impl ApprovalHandler for Router {
    async fn ask(&self, req: &ApprovalRequest) -> Decision {
        let routes = &self.0;
        let target = routes
            .store
            .get()
            .and_then(|s| s.run_deliver_to(&req.run_id).ok().flatten());
        if let Some((gateway, chat)) = target.as_deref().and_then(|t| t.split_once(':'))
            && let Some(g) = routes.gateways.get().and_then(|gs| gs.get(gateway))
        {
            return g.ask_approval(chat, req).await;
        }
        routes.api.ask(req).await
    }
}

pub struct Manager {
    pub rt: Arc<Runtime>,
    pub routes: Arc<Routes>,
    active: Mutex<HashMap<String, Active>>,
}

impl Manager {
    pub fn new(rt: Arc<Runtime>, routes: Arc<Routes>) -> Arc<Self> {
        Arc::new(Self {
            rt,
            routes,
            active: Mutex::new(HashMap::new()),
        })
    }

    fn gateway(&self, name: &str) -> Option<Arc<dyn Gateway>> {
        self.routes
            .gateways
            .get()
            .and_then(|g| g.get(name))
            .cloned()
    }

    /// Start a run in `session` (creating one from `source` if `None`),
    /// replacing any run in progress there.
    pub async fn start(
        self: &Arc<Self>,
        session: Option<&str>,
        model: Option<&str>,
        text: &str,
        source: &str,
        trigger: &str,
        deliver_to: Option<&str>,
    ) -> anyhow::Result<crate::runtime::StartedRun> {
        if let Some(s) = session {
            self.stop_session(s, true).await;
        }
        let started = self.rt.start_run(session, model, text, source, trigger)?;
        if let Some(t) = deliver_to {
            self.rt.store.set_run_deliver_to(&started.run_id, t)?;
        }
        self.spawn(&started.session_id, &started.run_id, false);
        Ok(started)
    }

    /// Drive `run_id` on a background task, then deliver its result.
    pub fn spawn(self: &Arc<Self>, session_id: &str, run_id: &str, resume: bool) {
        let cancel = CancellationToken::new();
        let (done_tx, done_rx) = watch::channel(false);
        if let Ok(mut a) = self.active.lock() {
            a.insert(
                session_id.to_owned(),
                Active {
                    run_id: run_id.to_owned(),
                    cancel: cancel.clone(),
                    started: Instant::now(),
                    done: done_rx,
                },
            );
        }
        let (me, session_id, run_id) = (self.clone(), session_id.to_owned(), run_id.to_owned());
        tokio::spawn(async move {
            let result = if resume {
                me.rt.agent.resume(&run_id, cancel).await
            } else {
                me.rt.agent.drive(&run_id, cancel).await
            };
            if let Ok(mut a) = me.active.lock()
                && a.get(&session_id).is_some_and(|x| x.run_id == run_id)
            {
                a.remove(&session_id);
            }
            let _ = done_tx.send(true);
            match result {
                Ok(outcome) => me.deliver(&outcome).await,
                Err(e) => tracing::error!(run = %run_id, error = %e, "run could not be driven"),
            }
        });
    }

    async fn deliver(&self, outcome: &RunOutcome) {
        let Ok(Some(target)) = self.rt.store.run_deliver_to(&outcome.run_id) else {
            return;
        };
        let Some((name, chat)) = target.split_once(':') else {
            return;
        };
        let Some(g) = self.gateway(name) else {
            tracing::warn!(target, "no gateway to deliver to");
            return;
        };
        let text = match outcome.status {
            RunStatus::Completed => outcome.final_text.clone(),
            RunStatus::Failed => format!(
                "Failed: {}",
                outcome.error.as_deref().unwrap_or("unknown error")
            ),
            // Interrupted runs were stopped on purpose; the gateway says so.
            RunStatus::Interrupted | RunStatus::Running => return,
        };
        if let Err(e) = g.deliver(chat, Some(&outcome.run_id), &text).await {
            tracing::warn!(target, error = %e, "delivery failed");
        }
    }

    /// Cancel the run in `session`, optionally waiting (briefly) for it to stop.
    pub async fn stop_session(&self, session: &str, wait: bool) -> bool {
        let active = self
            .active
            .lock()
            .ok()
            .and_then(|a| a.get(session).map(|x| (x.cancel.clone(), x.done.clone())));
        let Some((cancel, mut done)) = active else {
            return false;
        };
        cancel.cancel();
        if wait {
            let _ = tokio::time::timeout(Duration::from_secs(5), done.wait_for(|d| *d)).await;
        }
        true
    }

    pub fn cancel_run(&self, run_id: &str) -> bool {
        let Ok(a) = self.active.lock() else {
            return false;
        };
        match a.values().find(|x| x.run_id == run_id) {
            Some(x) => {
                x.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// `(session_id, run_id)` for every run in progress.
    pub fn active_runs(&self) -> Vec<(String, String)> {
        self.active
            .lock()
            .map(|a| {
                a.iter()
                    .map(|(s, x)| (s.clone(), x.run_id.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn is_active(&self, run_id: &str) -> bool {
        self.active
            .lock()
            .is_ok_and(|a| a.values().any(|x| x.run_id == run_id))
    }

    fn chat_session(&self, source: &str, chat: &str) -> anyhow::Result<tern_store::SessionRow> {
        if let Some(s) = self.rt.store.latest_session_for(source, chat)? {
            return Ok(s);
        }
        Ok(self.rt.store.create_session_with_ref(
            &tern_core::agent::new_id(),
            source,
            chat,
            None,
        )?)
    }
}

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

/// The manager as gateways see it.
pub struct Chats(pub Arc<Manager>);

#[async_trait]
impl ChatHost for Chats {
    async fn send(&self, source: &str, chat: &str, text: &str) -> Result<String, String> {
        let session = self.0.chat_session(source, chat).map_err(err)?;
        let target = format!("{source}:{chat}");
        let started = self
            .0
            .start(Some(&session.id), None, text, source, "user", Some(&target))
            .await
            .map_err(err)?;
        Ok(started.run_id)
    }

    async fn new_session(&self, source: &str, chat: &str) -> Result<(), String> {
        if let Ok(Some(old)) = self.0.rt.store.latest_session_for(source, chat) {
            self.0.stop_session(&old.id, true).await;
        }
        let model = self
            .0
            .rt
            .store
            .latest_session_for(source, chat)
            .ok()
            .flatten()
            .and_then(|s| s.model);
        self.0
            .rt
            .store
            .create_session_with_ref(&tern_core::agent::new_id(), source, chat, model.as_deref())
            .map(|_| ())
            .map_err(err)
    }

    async fn model(&self, source: &str, chat: &str, set: Option<&str>) -> Result<String, String> {
        let session = self.0.chat_session(source, chat).map_err(err)?;
        if let Some(m) = set {
            self.0.rt.check_model(m).map_err(err)?;
            self.0
                .rt
                .store
                .set_session_model(&session.id, m)
                .map_err(err)?;
            return Ok(m.to_owned());
        }
        self.0
            .rt
            .pick_model(None, session.model.as_deref())
            .map(|m| m.to_string())
            .map_err(err)
    }

    async fn stop(&self, source: &str, chat: &str) -> bool {
        match self.0.rt.store.latest_session_for(source, chat) {
            Ok(Some(s)) => self.0.stop_session(&s.id, false).await,
            _ => false,
        }
    }

    async fn status(&self, source: &str, chat: &str) -> String {
        let session = self
            .0
            .rt
            .store
            .latest_session_for(source, chat)
            .ok()
            .flatten();
        let model = self
            .0
            .rt
            .pick_model(None, session.as_ref().and_then(|s| s.model.as_deref()))
            .map(|m| m.to_string())
            .unwrap_or_else(|_| "none configured".into());
        let running = session.as_ref().and_then(|s| {
            self.0.active.lock().ok().and_then(|a| {
                a.get(&s.id)
                    .map(|x| (x.run_id.clone(), x.started.elapsed()))
            })
        });
        match running {
            Some((run, t)) => format!("Working for {}s (run {run}). Model: {model}.", t.as_secs()),
            None => format!("Idle. Model: {model}."),
        }
    }

    fn events(&self) -> broadcast::Receiver<AgentEvent> {
        self.0.rt.agent.subscribe()
    }
}
