//! Runs in the daemon: one active run per session, background driving,
//! delivery of results, and routing approvals to whoever can answer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tern_core::agent::{RunOutcome, RunStatus};
use tern_core::approval::{
    ApprovalHandler, ApprovalRequest, AuthPrompt, Decision, SecretAsk, SecretRequest, SecretValues,
};
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
    /// `read`, `write` or `destructive`; older daemons send none (`write`).
    #[serde(default)]
    pub effect: tern_core::tool::Effect,
    pub pattern: Option<String>,
    /// Set for a sub-agent's call: its parent run, its label, and the
    /// session of the top of its tree (where the user is looking).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<String>,
    /// Set when the run is tainted: why. Show it as "this run read untrusted
    /// content (…)"; grants don't apply and "always" isn't offered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taint: Option<String>,
    /// Set when it asks only because of the tainted host limit: the new
    /// host. Answers may then also be `run` (all sites for this run) or
    /// `host` (always allow this host).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_host: Option<String>,
    /// Set when a python script's own tool call asks: "from a python script
    /// (call 7 of this script)", then a newline and the script.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// The channel of the session at the top of the run's tree, and its
    /// agent (agent profiles §3): an agent's DM mirrors the card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Set when scoped grants may be offered: what they would cover. Answers
    /// may then be `run` (this run), `session` (this conversation) or
    /// `always` (for good), each only for exactly this key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<tern_core::approval::GrantOffer>,
    /// How many identical simultaneous requests this one card answers
    /// (1 when alone). A card whose count grows is sent again, same `id`.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub count: u32,
    /// Only a client on this host (the Unix socket) may answer: the call
    /// would run a command here outside the sandbox.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local_only: bool,
}

fn one() -> u32 {
    1
}

fn is_one(n: &u32) -> bool {
    *n == 1
}

/// Approvals for API clients: published on a channel, answered over HTTP.
#[derive(Default)]
pub struct ApiApprovals {
    pending: Mutex<HashMap<String, Card>>,
    tx: OnceLock<broadcast::Sender<ApiApproval>>,
}

/// One card for the user, answering every identical request that joined it.
struct Card {
    msg: ApiApproval,
    /// What makes a request identical to this card's: same run, call and
    /// shown details. Only then may one answer cover several requests.
    same: String,
    /// The requests behind the card: (request id, its asker).
    waiters: Vec<(String, oneshot::Sender<Decision>)>,
}

/// What must match for two pending requests to share a card: everything the
/// user sees and everything that decides what an answer means.
fn same_request(req: &ApprovalRequest) -> String {
    serde_json::json!([
        req.run_id,
        req.call.name,
        req.call.input,
        req.summary,
        req.effect.as_str(),
        req.pattern,
        req.grant,
        req.taint,
        req.egress_host,
        req.script,
        req.local_only,
    ])
    .to_string()
}

impl ApiApprovals {
    fn tx(&self) -> &broadcast::Sender<ApiApproval> {
        self.tx.get_or_init(|| broadcast::channel(64).0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ApiApproval> {
        self.tx().subscribe()
    }

    /// Ask attached clients; `Unanswered` at once if none is listening (the
    /// agent then queues a write in the outbox). The request leaves the
    /// pending list when the agent stops waiting.
    /// `root_session_id` is the session at the top of a sub-agent's tree.
    async fn ask(
        &self,
        req: &ApprovalRequest,
        root_session_id: Option<String>,
        at: Where,
    ) -> Decision {
        let (tx, rx) = oneshot::channel();
        let same = same_request(req);
        let msg = ApiApproval {
            id: req.id.clone(),
            run_id: req.run_id.clone(),
            session_id: req.session_id.clone(),
            tool: req.call.name.clone(),
            input: req.call.input.clone(),
            summary: req.summary.clone(),
            effect: req.effect,
            pattern: req.pattern.clone(),
            parent_run_id: req.parent_run_id.clone(),
            agent_label: req.agent_label.clone(),
            root_session_id: root_session_id.filter(|_| req.parent_run_id.is_some()),
            taint: req.taint.clone(),
            egress_host: req.egress_host.clone(),
            script: req.script.clone(),
            grant: req.grant.clone(),
            count: 1,
            local_only: req.local_only,
            channel_id: at.0,
            agent_id: at.1,
        };
        // An identical request already waiting shares its card: one answer
        // covers both, and clients see the count go up.
        let shown = {
            let Ok(mut p) = self.pending.lock() else {
                return Decision::Unanswered;
            };
            match p.values_mut().find(|c| c.same == same) {
                Some(card) => {
                    card.waiters.push((req.id.clone(), tx));
                    card.msg.count = card.waiters.len() as u32;
                    card.msg.clone()
                }
                None => {
                    p.insert(
                        req.id.clone(),
                        Card {
                            msg: msg.clone(),
                            same,
                            waiters: vec![(req.id.clone(), tx)],
                        },
                    );
                    msg
                }
            }
        };
        let _unlist = UnlistApproval(self, req.id.clone());
        if self.tx().send(shown).is_err() {
            return Decision::Unanswered;
        }
        rx.await.unwrap_or(Decision::Unanswered)
    }

    /// Whether an identical request is already waiting, so a new one joins
    /// its card rather than opening another.
    pub(crate) fn joins_card(&self, req: &ApprovalRequest) -> bool {
        let same = same_request(req);
        self.pending
            .lock()
            .is_ok_and(|p| p.values().any(|c| c.same == same))
    }

    /// Whether any client is attached to hear an approval.
    pub fn listening(&self) -> bool {
        self.tx().receiver_count() > 0
    }

    /// Answer a card: every request behind it gets `decision`. A scoped
    /// answer (`run`, `session`, `always`) also releases the other cards
    /// already waiting for exactly the same grant key (same tool, and the
    /// same run, conversation or anything for "always"): they would be
    /// granted by it on their next ask, so each is answered
    /// [`Decision::CoveredByGrant`] (the ledger and transcript say it was
    /// covered, not that anyone approved it). Only cards that offered a
    /// grant qualify, so never a tainted or destructive one, and a
    /// local-only card is left alone when the answer came from a remote
    /// device (`remote`).
    pub fn answer(&self, id: &str, decision: Decision, remote: bool) -> bool {
        let (card, released) = {
            let Ok(mut p) = self.pending.lock() else {
                return false;
            };
            let Some(card) = p.remove(id) else {
                return false;
            };
            let mut released = Vec::new();
            if let Some(g) = card.msg.grant.as_ref().filter(|_| {
                matches!(
                    decision,
                    Decision::AllowRun | Decision::AllowSession | Decision::AlwaysAllow
                )
            }) {
                let ids: Vec<String> = p
                    .iter()
                    .filter(|(_, c)| {
                        c.msg.tool == card.msg.tool
                            && !(remote && c.msg.local_only)
                            && c.msg.grant.as_ref().is_some_and(|o| o.key == g.key)
                            && match decision {
                                Decision::AllowRun => c.msg.run_id == card.msg.run_id,
                                Decision::AllowSession => c.msg.session_id == card.msg.session_id,
                                _ => true,
                            }
                    })
                    .map(|(k, _)| k.clone())
                    .collect();
                released.extend(ids.iter().filter_map(|k| p.remove(k)));
            }
            (card, released)
        };
        let send = |card: Card, d: Decision| {
            card.waiters
                .into_iter()
                .fold(false, |any, (_, tx)| tx.send(d).is_ok() || any)
        };
        for c in released {
            send(c, Decision::CoveredByGrant);
        }
        send(card, decision)
    }

    /// Whether `id` may only be answered from this host.
    pub fn is_local_only(&self, id: &str) -> bool {
        self.pending
            .lock()
            .ok()
            .and_then(|p| p.get(id).map(|c| c.msg.local_only))
            .unwrap_or(false)
    }

    /// Approvals still waiting for an answer, for clients that attach late.
    pub fn pending(&self) -> Vec<ApiApproval> {
        self.pending
            .lock()
            .map(|p| p.values().map(|c| c.msg.clone()).collect())
            .unwrap_or_default()
    }
}

/// Drops a request from its card when its asker stops waiting (answered,
/// timed out into the outbox, or cancelled); the card goes with its last one.
struct UnlistApproval<'a>(&'a ApiApprovals, String);

impl Drop for UnlistApproval<'_> {
    fn drop(&mut self) {
        let Ok(mut p) = self.0.pending.lock() else {
            return;
        };
        let card = p
            .iter_mut()
            .find(|(_, c)| c.waiters.iter().any(|(r, _)| r == &self.1))
            .map(|(k, c)| {
                c.waiters.retain(|(r, _)| r != &self.1);
                c.msg.count = c.waiters.len().max(1) as u32;
                (k.clone(), c.waiters.is_empty())
            });
        if let Some((k, true)) = card {
            p.remove(&k);
        }
    }
}

/// A secret request waiting for an API client to answer (never with the
/// values: those only travel in the answer).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ApiSecretRequest {
    pub id: String,
    pub run_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub secrets: Vec<SecretAsk>,
    /// The agent's words.
    pub reason: String,
    pub replace: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<String>,
    /// As on [`ApiApproval`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

type PendingSecrets = Mutex<HashMap<String, (ApiSecretRequest, oneshot::Sender<SecretValues>)>>;

/// Secret requests for API clients. Unlike approvals they wait when no
/// client is attached: the user is notified and may open the app, and the
/// tool's own timeout ends the wait.
#[derive(Default)]
pub struct ApiSecretRequests {
    pending: Arc<PendingSecrets>,
    tx: OnceLock<broadcast::Sender<ApiSecretRequest>>,
}

/// Drops a request from the pending list when its asker stops waiting.
struct Unlist(Arc<PendingSecrets>, String);

impl Drop for Unlist {
    fn drop(&mut self) {
        if let Ok(mut p) = self.0.lock() {
            p.remove(&self.1);
        }
    }
}

impl ApiSecretRequests {
    fn tx(&self) -> &broadcast::Sender<ApiSecretRequest> {
        self.tx.get_or_init(|| broadcast::channel(64).0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ApiSecretRequest> {
        self.tx().subscribe()
    }

    async fn ask(
        &self,
        req: &SecretRequest,
        root_session_id: Option<String>,
        at: Where,
    ) -> SecretValues {
        let (tx, rx) = oneshot::channel();
        let msg = ApiSecretRequest {
            id: req.id.clone(),
            run_id: req.run_id.clone(),
            session_id: req.session_id.clone(),
            call_id: req.call_id.clone(),
            secrets: req.asks.clone(),
            reason: req.reason.clone(),
            replace: req.replace,
            parent_run_id: req.parent_run_id.clone(),
            agent_label: req.agent_label.clone(),
            root_session_id: root_session_id.filter(|_| req.parent_run_id.is_some()),
            channel_id: at.0,
            agent_id: at.1,
        };
        if let Ok(mut p) = self.pending.lock() {
            p.insert(req.id.clone(), (msg.clone(), tx));
        }
        let _unlist = Unlist(self.pending.clone(), req.id.clone());
        // Nobody listening is fine: late clients find it in `pending`.
        let _ = self.tx().send(msg);
        rx.await.unwrap_or_default()
    }

    /// Answer `id` with `values`. Every name must be one the request asked
    /// for, so a request for `FOO` can't be used to write another secret.
    pub fn answer(&self, id: &str, values: SecretValues) -> Result<(), AnswerError> {
        let mut p = self.pending.lock().map_err(|_| AnswerError::NotFound)?;
        let Some((req, _)) = p.get(id) else {
            return Err(AnswerError::NotFound);
        };
        if let Some(extra) = values
            .keys()
            .find(|k| !req.secrets.iter().any(|a| &a.name == *k))
        {
            return Err(AnswerError::NotAsked(extra.clone()));
        }
        let Some((_, tx)) = p.remove(id) else {
            return Err(AnswerError::NotFound);
        };
        tx.send(values).map_err(|_| AnswerError::NotFound)
    }

    pub fn pending(&self) -> Vec<ApiSecretRequest> {
        self.pending
            .lock()
            .map(|p| p.values().map(|(r, _)| r.clone()).collect())
            .unwrap_or_default()
    }
}

/// A sign-in card waiting for an API client (tooling spec §6.7.3–6.7.4).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ApiAuthPrompt {
    #[serde(flatten)]
    pub prompt: AuthPrompt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// What `/v1/events` carries about sign-in cards.
#[derive(Clone)]
pub enum AuthPromptEvent {
    Open(Box<ApiAuthPrompt>),
    /// Answered, or the sign-in finished without an answer: close it.
    Closed(String),
}

type PendingAuth = Mutex<HashMap<String, (ApiAuthPrompt, oneshot::Sender<String>)>>;

/// Sign-in cards for API clients. Like secret requests they wait when no
/// client is attached, until the tool's own timeout.
#[derive(Default)]
pub struct ApiAuthPrompts {
    pending: Arc<PendingAuth>,
    tx: OnceLock<broadcast::Sender<AuthPromptEvent>>,
}

/// Drops a card from the pending list, and tells clients to close it, when
/// its asker stops waiting.
struct UnlistAuth(Arc<PendingAuth>, String, broadcast::Sender<AuthPromptEvent>);

impl Drop for UnlistAuth {
    fn drop(&mut self) {
        if let Ok(mut p) = self.0.lock() {
            p.remove(&self.1);
        }
        let _ = self.2.send(AuthPromptEvent::Closed(self.1.clone()));
    }
}

impl ApiAuthPrompts {
    fn tx(&self) -> &broadcast::Sender<AuthPromptEvent> {
        self.tx.get_or_init(|| broadcast::channel(64).0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AuthPromptEvent> {
        self.tx().subscribe()
    }

    /// Show `req` and wait for an answer it accepts.
    pub async fn ask(
        &self,
        req: &AuthPrompt,
        root_session_id: Option<String>,
        at: Where,
    ) -> Option<String> {
        let (tx, rx) = oneshot::channel();
        let msg = ApiAuthPrompt {
            prompt: req.clone(),
            root_session_id: root_session_id.filter(|_| req.parent_run_id.is_some()),
            channel_id: at.0,
            agent_id: at.1,
        };
        if let Ok(mut p) = self.pending.lock() {
            p.insert(req.id.clone(), (msg.clone(), tx));
        }
        let _unlist = UnlistAuth(self.pending.clone(), req.id.clone(), self.tx().clone());
        let _ = self.tx().send(AuthPromptEvent::Open(Box::new(msg)));
        rx.await.ok()
    }

    /// Answer card `id`. Only an answer the card offers is taken.
    pub fn answer(&self, id: &str, answer: &str) -> Result<(), AnswerError> {
        let mut p = self.pending.lock().map_err(|_| AnswerError::NotFound)?;
        let Some((req, _)) = p.get(id) else {
            return Err(AnswerError::NotFound);
        };
        if !req.prompt.accepts(answer) {
            return Err(AnswerError::NotAsked(answer.to_owned()));
        }
        let Some((_, tx)) = p.remove(id) else {
            return Err(AnswerError::NotFound);
        };
        tx.send(answer.to_owned())
            .map_err(|_| AnswerError::NotFound)
    }

    pub fn pending(&self) -> Vec<ApiAuthPrompt> {
        self.pending
            .lock()
            .map(|p| p.values().map(|(r, _)| r.clone()).collect())
            .unwrap_or_default()
    }
}

#[derive(Debug)]
pub enum AnswerError {
    NotFound,
    /// A name the request didn't ask for.
    NotAsked(String),
}

/// Late-bound pieces the approval router needs (the runtime is built with
/// the router, so they can't be passed to its constructor).
#[derive(Default)]
pub struct Routes {
    pub store: OnceLock<tern_store::Store>,
    pub gateways: OnceLock<HashMap<String, Arc<dyn Gateway>>>,
    pub api: ApiApprovals,
    pub secrets: ApiSecretRequests,
    pub auth: ApiAuthPrompts,
    pub lifecycle: Lifecycle,
    /// Set once the daemon's manager exists, to announce outbox items.
    pub manager: OnceLock<Weak<Manager>>,
}

/// What `/v1/status` and `/v1/shutdown` need from the daemon (T8a).
pub struct Lifecycle {
    pub started: Instant,
    /// Cancelled by `POST /v1/shutdown`; `daemon::serve` then stops as it
    /// does on SIGTERM.
    pub shutdown: CancellationToken,
    /// Each gateway's polling task, to report whether it is still up.
    pub gateway_tasks: Mutex<Vec<(String, tokio::task::JoinHandle<()>)>>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            shutdown: CancellationToken::new(),
            gateway_tasks: Mutex::new(Vec::new()),
        }
    }
}

/// Sends each approval to the chat the run's tree reports to, else to API
/// clients. A sub-agent asks where its root run would.
pub struct Router(pub Arc<Routes>);

/// A gateway and the chat on it a run tree reports to.
type ChatTarget = (Arc<dyn Gateway>, String);

/// The channel and agent of a run tree's root session.
pub(crate) type Where = (Option<String>, Option<String>);

impl Router {
    /// The session at the top of `run_id`'s tree, and the gateway chat that
    /// tree reports to, if any.
    fn place(&self, run_id: &str) -> (Option<String>, Option<ChatTarget>) {
        let routes = &self.0;
        let root = routes.store.get().and_then(|s| {
            let root = s.root_run(run_id).ok()?;
            s.run(&root).ok().flatten()
        });
        let target = root.as_ref().and_then(|r| {
            let s = routes.store.get()?;
            s.run_deliver_to(&r.id).ok().flatten()
        });
        let chat = target.as_deref().and_then(|t| {
            let (gateway, chat) = t.split_once(':')?;
            let g = routes.gateways.get()?.get(gateway)?.clone();
            Some((g, chat.to_owned()))
        });
        (root.map(|r| r.session_id), chat)
    }

    /// The conversation the run tree is in: where its cards wait.
    fn at(&self, root_session: Option<&str>) -> Where {
        let (Some(store), Some(id)) = (self.0.store.get(), root_session) else {
            return (None, None);
        };
        super::waiting::conversation_of(store, id)
    }

    fn manager(&self) -> Option<Arc<Manager>> {
        self.0.manager.get().and_then(Weak::upgrade)
    }

    /// Ask API clients, with a `needs_you` post in the run's conversation
    /// while it waits (none in Home, or when no client listens: then it
    /// goes to the outbox at once, which posts its own).
    async fn ask_api(&self, req: &ApprovalRequest, root: Option<String>, at: Where) -> Decision {
        let waiting = match self.manager() {
            Some(m) if self.0.api.listening() && !self.0.api.joins_card(req) => {
                super::waiting::open_approval(&m, req, &at)
            }
            _ => None,
        };
        let d = self.0.api.ask(req, root, at).await;
        super::waiting::close_approval(waiting, &d);
        d
    }
}

/// Updates a gateway's secret notice when the request ends, however it ends.
struct Notice {
    gateway: Arc<dyn Gateway>,
    chat: String,
    handle: String,
    text: Option<String>,
}

impl Drop for Notice {
    fn drop(&mut self) {
        let text = self
            .text
            .take()
            .unwrap_or_else(|| "🔑 No longer waiting for this secret.".into());
        let (g, chat, handle) = (self.gateway.clone(), self.chat.clone(), self.handle.clone());
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move { g.secret_request_done(&chat, &handle, &text).await });
        }
    }
}

#[async_trait]
impl ApprovalHandler for Router {
    /// A local-only approval never goes to a chat: the TUI or desktop app
    /// on this host answers it.
    async fn ask(&self, req: &ApprovalRequest) -> Decision {
        // A routine inside its quiet hours doesn't ping the chat: an app
        // may answer, else a write goes to the outbox, which its morning
        // delivery lists (spec W4).
        if self
            .0
            .manager
            .get()
            .and_then(Weak::upgrade)
            .is_some_and(|m| m.routine_quiet_now(&req.run_id))
        {
            let (root, _) = self.place(&req.run_id);
            let at = self.at(root.as_deref());
            return self.ask_api(req, root, at).await;
        }
        match self.place(&req.run_id) {
            (_, Some((g, chat))) if !req.local_only => g.ask_approval(&chat, req).await,
            (root, _) => {
                let at = self.at(root.as_deref());
                self.ask_api(req, root, at).await
            }
        }
    }

    /// The daemon executes approved outbox items.
    fn can_defer(&self) -> bool {
        true
    }

    async fn deferred(&self, _req: &ApprovalRequest, id: i64) {
        if let Some(m) = self.0.manager.get().and_then(Weak::upgrade) {
            m.announce_outbox(id).await;
        }
    }

    /// API clients always get the prompt. A chat only gets a pointer to
    /// them, since a value typed there would stay in its history.
    async fn ask_secrets(&self, req: &SecretRequest) -> Option<SecretValues> {
        let (root, chat) = self.place(&req.run_id);
        let mut notice = None;
        if let Some((g, chat)) = chat
            && let Some(handle) = g.secret_request_notice(&chat, req).await
        {
            notice = Some(Notice {
                gateway: g,
                chat,
                handle,
                text: None,
            });
        }
        let at = self.at(root.as_deref());
        let waiting = self
            .manager()
            .and_then(|m| super::waiting::open_secret(&m, req, &at));
        let values = self.0.secrets.ask(req, root, at).await;
        super::waiting::close_secret(waiting, values.values().any(|v| !v.trim().is_empty()));
        if let Some(n) = &mut notice {
            // What the tool will save: a value that cleans up to nothing
            // (`""`, a bare `Bearer `) counts as skipped there too.
            let saved = |name: &str| {
                values.get(name).is_some_and(|v| {
                    !tern_core::secrets::keys::normalize_key(v)
                        .unwrap_or_else(|_| v.trim().to_owned())
                        .is_empty()
                })
            };
            let names = |set: bool| -> Vec<&str> {
                req.asks
                    .iter()
                    .filter(|a| saved(&a.name) == set)
                    .map(|a| a.name.as_str())
                    .collect()
            };
            let (saved, skipped) = (names(true), names(false));
            let mut text = String::from("🔑 ");
            if !saved.is_empty() {
                text.push_str(&format!("✓ {} saved. ", saved.join(", ")));
            }
            if !skipped.is_empty() {
                text.push_str(&format!("{} skipped.", skipped.join(", ")));
            }
            n.text = Some(text.trim_end().to_owned());
        }
        Some(values)
    }

    /// API clients get the card; a chat gets a pointer to them.
    async fn ask_auth(&self, req: &AuthPrompt) -> Option<String> {
        let (root, chat) = self.place(&req.run_id);
        let mut notice = None;
        if let Some((g, chat)) = chat
            && let Some(handle) = g.auth_prompt_notice(&chat, req).await
        {
            notice = Some(Notice {
                gateway: g,
                chat,
                handle,
                text: None,
            });
        }
        let at = self.at(root.as_deref());
        let answer = self.0.auth.ask(req, root, at).await;
        if let Some(n) = &mut notice {
            n.text = Some(match answer.as_deref() {
                Some("cancel") => "🔐 Cancelled.".into(),
                Some(other) => format!("🔐 Chose: {other}."),
                None => "🔐 Done.".into(),
            });
        }
        answer
    }
}

/// A watch run's input: the routine's prompt, then the events, wrapped as
/// untrusted.
pub(crate) fn watch_input(
    prompt: &str,
    watch: &str,
    events: &[tern_store::RoutineEvent],
) -> String {
    let (what, body) = match events {
        [one] => ("an event".to_owned(), one.payload.clone()),
        many => (
            format!("{} events", many.len()),
            format!(
                "[\n{}\n]",
                many.iter()
                    .map(|e| e.payload.as_str())
                    .collect::<Vec<_>>()
                    .join(",\n")
            ),
        ),
    };
    format!(
        "{prompt}\n\nThe watch {watch} reported {what}:\n{}",
        tern_core::agent::taint::wrap(&format!("watch:{watch}"), &body)
    )
}

/// A webhook routine's run input (T1c): its prompt, then the delivery,
/// wrapped as untrusted. Webhook events never batch, so there is one.
pub(crate) fn webhook_input(prompt: &str, events: &[tern_store::RoutineEvent]) -> String {
    let body = events
        .iter()
        .map(|e| e.payload.as_str())
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "{prompt}\n\nA webhook delivered this event:\n{}",
        tern_core::agent::taint::wrap("webhook", &body)
    )
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

    pub(super) fn gateway(&self, name: &str) -> Option<Arc<dyn Gateway>> {
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
        self.start_tainted(session, model, text, source, trigger, deliver_to, None)
            .await
    }

    /// [`Manager::start`] for untrusted input: the run starts tainted for
    /// `taint` (tooling spec §5.3).
    #[allow(clippy::too_many_arguments)]
    pub async fn start_tainted(
        self: &Arc<Self>,
        session: Option<&str>,
        model: Option<&str>,
        text: &str,
        source: &str,
        trigger: &str,
        deliver_to: Option<&str>,
        taint: Option<&str>,
    ) -> anyhow::Result<crate::runtime::StartedRun> {
        if let Some(s) = session {
            self.stop_session(s, true).await;
        }
        let started = self
            .rt
            .start_run_tainted(session, model, text, source, trigger, taint)?;
        if let Some(t) = deliver_to {
            self.rt.store.set_run_deliver_to(&started.run_id, t)?;
        }
        self.spawn(&started.session_id, &started.run_id, false);
        Ok(started)
    }

    /// The run working in `session`, if one is.
    pub(super) fn active_run_in(&self, session: &str) -> Option<String> {
        self.active
            .lock()
            .ok()?
            .get(session)
            .map(|x| x.run_id.clone())
    }

    /// If a run is working in `session`, give it `text` as a message from
    /// the user (read at its next model call) instead of stopping it, and
    /// return that run's id and the mail id. Checked and posted under the
    /// lock that a finishing run takes to leave, so the message is either
    /// read by the run or found by [`Manager::follow_up_queued`].
    pub fn queue_message(
        &self,
        session: &str,
        text: &str,
    ) -> anyhow::Result<Option<(String, i64)>> {
        let Ok(active) = self.active.lock() else {
            return Ok(None);
        };
        let Some(run_id) = active.get(session).map(|x| x.run_id.clone()) else {
            return Ok(None);
        };
        let key = format!("user:{run_id}:{}", tern_core::agent::new_id());
        self.rt.store.post_mail(&tern_store::NewMail {
            to_run_id: &run_id,
            from_run_id: None,
            kind: "message",
            body: &serde_json::json!({ "text": text }).to_string(),
            dedupe_key: &key,
        })?;
        let id = self
            .rt
            .store
            .mail_by_key(&key)?
            .map(|m| m.id)
            .unwrap_or_default();
        tracing::info!(run = %run_id, "queued the user's message for the run in progress");
        Ok(Some((run_id, id)))
    }

    /// Start a run of schedule `s` in its session, with its token cap.
    /// Daily caps are the scheduler's job; "Run now" doesn't check them.
    pub async fn start_scheduled(
        self: &Arc<Self>,
        s: &tern_store::ScheduleRow,
    ) -> anyhow::Result<crate::runtime::StartedRun> {
        self.stop_session(&s.session_id, true).await;
        let started = self
            .rt
            .start_run(Some(&s.session_id), None, &s.prompt, "cron", "cron")?;
        self.drive_routine_run(s, started)
    }

    /// Start a run of watch routine `s` for `events` (one, or a batch).
    /// The events are untrusted input: the run starts tainted (tooling spec
    /// §3.1, §5.3), and they reach the model wrapped. Its trigger is
    /// `cron`, like every routine's run.
    pub async fn start_watch_run(
        self: &Arc<Self>,
        s: &tern_store::ScheduleRow,
        events: &[tern_store::RoutineEvent],
    ) -> anyhow::Result<crate::runtime::StartedRun> {
        let (text, reason) = if s.trigger() == "webhook" {
            (
                webhook_input(&s.prompt, events),
                super::hooks::TAINT.to_owned(),
            )
        } else {
            let watch = s.watch.as_deref().unwrap_or("watch");
            (
                watch_input(&s.prompt, watch, events),
                format!("started by watch {watch}"),
            )
        };
        let started = self.rt.start_run_tainted(
            Some(&s.session_id),
            None,
            &text,
            "cron",
            "cron",
            Some(&reason),
        )?;
        self.drive_routine_run(s, started)
    }

    /// A routine's run, just created: its delivery target, autonomy and
    /// token cap, then drive it.
    fn drive_routine_run(
        self: &Arc<Self>,
        s: &tern_store::ScheduleRow,
        started: crate::runtime::StartedRun,
    ) -> anyhow::Result<crate::runtime::StartedRun> {
        if let Some(t) = &s.deliver_to {
            self.rt.store.set_run_deliver_to(&started.run_id, t)?;
        }
        if let Some(a) = s.autonomy.as_deref() {
            // The routine's own profile, still under its agent's ceiling.
            let agent = self
                .rt
                .store
                .agent(s.agent())?
                .unwrap_or_else(|| tern_store::AgentRow {
                    id: tern_store::DEFAULT_AGENT.into(),
                    ..tern_store::AgentRow::default()
                });
            let level = self
                .rt
                .autonomy_for(&agent, tern_core::approval::Autonomy::parse(a));
            self.rt
                .store
                .set_run_autonomy(&started.run_id, level.as_str())?;
        }
        let cap = s.tokens_per_run.or(self.rt.live().schedules.tokens_per_run);
        if let Some(cap) = cap {
            self.rt.store.set_run_budget_tokens(&started.run_id, cap)?;
        }
        self.spawn(&started.session_id, &started.run_id, false);
        Ok(started)
    }

    /// Drive `run_id` on a background task, then deliver its result. False
    /// (and nothing happens) if it is already being driven.
    pub fn spawn(self: &Arc<Self>, session_id: &str, run_id: &str, resume: bool) -> bool {
        let cancel = CancellationToken::new();
        let (done_tx, done_rx) = watch::channel(false);
        if let Ok(mut a) = self.active.lock() {
            if a.values().any(|x| x.run_id == run_id) {
                return false;
            }
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
            me.delegation_changed(&run_id);
            let result = if resume {
                me.rt.agent.resume(&run_id, cancel).await
            } else {
                me.rt.agent.drive(&run_id, cancel).await
            };
            // Before the run stops counting as active, so `/wait` sees it.
            if result.is_ok()
                && let Err(e) = me.post_run_result(&run_id)
            {
                tracing::warn!(run = %run_id, error = %e, "writing the run's post failed");
            }
            if result.is_ok()
                && let Err(e) = me.maybe_offer_skill(&run_id)
            {
                tracing::warn!(run = %run_id, error = %e, "offering the run as a skill failed");
            }
            if let Ok(mut a) = me.active.lock()
                && a.get(&session_id).is_some_and(|x| x.run_id == run_id)
            {
                a.remove(&session_id);
            }
            me.delegation_changed(&run_id);
            let _ = done_tx.send(true);
            if let Err(e) = me.follow_up_queued(&run_id).await {
                tracing::warn!(run = %run_id, error = %e, "answering queued messages failed");
            }
            match result {
                Ok(outcome) => me.deliver(&outcome).await,
                Err(e) => tracing::error!(run = %run_id, error = %e, "run could not be driven"),
            }
        });
        true
    }

    async fn deliver(&self, outcome: &RunOutcome) {
        let Ok(Some(run)) = self.rt.store.run(&outcome.run_id) else {
            return;
        };
        // Sub-agents report to their parent, never to a chat.
        if run.parent_run_id.is_some() {
            return;
        }
        if outcome.quiet {
            tracing::info!(run = %run.id, "scheduled run answered NO_REPLY; nothing delivered");
            return;
        }
        let Ok(Some(target)) = self.rt.store.run_deliver_to(&outcome.run_id) else {
            return;
        };
        let text = match outcome.status {
            // What it left in the outbox, so a routine's morning delivery
            // says what waits (spec W4).
            RunStatus::Completed => {
                let pending = super::outbox::pending_note(&self.rt.store, &run.id);
                if pending.is_empty() {
                    outcome.final_text.clone()
                } else {
                    format!("{}{pending}\nApprove with /outbox.", outcome.final_text)
                }
            }
            RunStatus::Failed => format!(
                "Failed: {}",
                outcome.error.as_deref().unwrap_or("unknown error")
            ),
            // Interrupted runs were stopped on purpose; the gateway says so.
            RunStatus::Interrupted | RunStatus::Running => return,
        };
        let schedule = (run.trigger == "cron")
            .then(|| {
                self.rt
                    .store
                    .schedule_for_session(&run.session_id)
                    .ok()
                    .flatten()
            })
            .flatten();
        match schedule {
            Some(s) => {
                let failed = outcome.status == RunStatus::Failed;
                self.deliver_routine(&s, &run, &target, &text, failed).await
            }
            None => {
                if let Err(e) = self.send_to(&target, Some(&run.id), &text).await {
                    tracing::warn!(target, error = %e, "delivery failed");
                }
            }
        }
    }

    /// Push a routine's result by its initiative (tooling spec §4.1): the
    /// routine's own level or the default, moved by the global dial.
    /// `silent` sends nothing, `digest` holds it for the next digest time,
    /// `notify` asks the gate model and pushes only on yes (else digest),
    /// `interrupt` pushes (quiet hours still hold it unless `urgent`). A
    /// failed run isn't put to the gate model: `notify` pushes it.
    async fn deliver_routine(
        &self,
        s: &tern_store::ScheduleRow,
        run: &tern_store::RunRow,
        target: &str,
        text: &str,
        failed: bool,
    ) {
        use tern_core::initiative::Initiative;
        let cfg = self.rt.live().schedules;
        let own = s
            .initiative
            .as_deref()
            .and_then(Initiative::parse)
            .unwrap_or(cfg.initiative);
        let level = cfg.initiative_dial.apply(own);
        match level {
            Initiative::Silent => {
                tracing::info!(schedule = %s.id, run = %run.id, "silent routine: posted, not pushed");
            }
            Initiative::Digest => self.hold_for_digest(s, target, &run.id, text),
            Initiative::Notify if failed => {
                self.hold_or_send(s, target, Some(&run.id), text).await;
            }
            Initiative::Notify => {
                let model = run.model.as_deref().unwrap_or_default();
                match crate::routines::ask_gate(&self.rt, &s.prompt, model, text).await {
                    Ok(a) if a.interrupt => {
                        tracing::info!(schedule = %s.id, headline = %a.headline, "gate model: interrupt");
                        let text = if a.headline.is_empty() {
                            text.to_owned()
                        } else {
                            format!("{}\n\n{text}", a.headline)
                        };
                        self.hold_or_send(s, target, Some(&run.id), &text).await;
                    }
                    Ok(_) => {
                        tracing::info!(schedule = %s.id, "gate model: not now; into the digest");
                        self.hold_for_digest(s, target, &run.id, text);
                    }
                    Err(e) => {
                        tracing::warn!(schedule = %s.id, error = %e, "gate model failed; into the digest");
                        self.hold_for_digest(s, target, &run.id, text);
                    }
                }
            }
            Initiative::Interrupt => self.hold_or_send(s, target, Some(&run.id), text).await,
        }
    }

    /// Hold a routine's result until the next digest time (or the end of
    /// quiet hours, if that time falls in them).
    fn hold_for_digest(&self, s: &tern_store::ScheduleRow, target: &str, run_id: &str, text: &str) {
        let cfg = self.rt.live().schedules;
        let quiet = s
            .quiet_hours
            .as_deref()
            .and_then(|q| q.parse::<tern_core::quiet::QuietHours>().ok())
            .unwrap_or(cfg.quiet_hours);
        let at = cfg.digest_times.next_after(chrono::Local::now());
        let at = quiet.release_at(at).unwrap_or(at);
        let label = super::scheduler::title(s);
        match self.rt.store.hold_delivery_as(
            target,
            Some(run_id),
            Some(&label),
            text,
            at.timestamp_millis(),
            "digest",
        ) {
            Ok(()) => tracing::info!(schedule = %s.id, until = %at, "held for the digest"),
            Err(e) => tracing::warn!(schedule = %s.id, error = %e, "could not hold for the digest"),
        }
    }

    /// Whether `run_id`'s tree is a routine's, inside its quiet hours now
    /// (and not `urgent`): its chat isn't pinged.
    pub fn routine_quiet_now(&self, run_id: &str) -> bool {
        let store = &self.rt.store;
        let Some(root) = store
            .root_run(run_id)
            .ok()
            .and_then(|r| store.run(&r).ok().flatten())
            .filter(|r| r.trigger == "cron")
        else {
            return false;
        };
        let Some(s) = store.schedule_for_session(&root.session_id).ok().flatten() else {
            return false;
        };
        let quiet = s
            .quiet_hours
            .as_deref()
            .and_then(|q| q.parse::<tern_core::quiet::QuietHours>().ok())
            .unwrap_or(self.rt.live().schedules.quiet_hours);
        !s.urgent && quiet.release_at(chrono::Local::now()).is_some()
    }

    /// Deliver `text` for schedule `s` to its target, if it has one.
    pub async fn deliver_for_schedule(
        &self,
        s: &tern_store::ScheduleRow,
        run_id: Option<&str>,
        text: &str,
    ) {
        if let Some(target) = &s.deliver_to {
            self.hold_or_send(s, target, run_id, text).await;
        }
    }

    /// Send now, or hold until the schedule's quiet hours end (held
    /// deliveries are in the database, so a restart keeps them).
    async fn hold_or_send(
        &self,
        s: &tern_store::ScheduleRow,
        target: &str,
        run_id: Option<&str>,
        text: &str,
    ) {
        let quiet = s
            .quiet_hours
            .as_deref()
            .and_then(|q| q.parse::<tern_core::quiet::QuietHours>().ok())
            .unwrap_or(self.rt.live().schedules.quiet_hours);
        let release = (!s.urgent)
            .then(|| quiet.release_at(chrono::Local::now()))
            .flatten();
        if let Some(at) = release {
            let label = super::scheduler::title(s);
            match self.rt.store.hold_delivery(
                target,
                run_id,
                Some(&label),
                text,
                at.timestamp_millis(),
            ) {
                Ok(()) => {
                    tracing::info!(schedule = %s.id, until = %at, "quiet hours: delivery held");
                    return;
                }
                Err(e) => tracing::warn!(error = %e, "could not hold delivery; sending now"),
            }
        }
        if let Err(e) = self.send_to(target, run_id, text).await {
            tracing::warn!(target, error = %e, "delivery failed");
        }
    }

    /// Send `text` to `gateway:chat`.
    pub async fn send_to(
        &self,
        target: &str,
        run_id: Option<&str>,
        text: &str,
    ) -> Result<(), String> {
        let (name, chat) = target
            .split_once(':')
            .ok_or_else(|| format!("bad delivery target {target:?}"))?;
        let g = self
            .gateway(name)
            .ok_or_else(|| format!("no gateway {name:?} to deliver to"))?;
        g.deliver(chat, run_id, text).await
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

    /// Cancel a run. A sub-agent is cancelled with its subtree (even while
    /// queued); a parent's children are cancelled when its drive ends.
    pub fn cancel_run(&self, run_id: &str) -> bool {
        if let Some(h) = &self.rt.subagents
            && h.cancel_run(run_id, "cancelled by the user")
        {
            return true;
        }
        self.cancel_active(run_id)
    }

    fn cancel_active(&self, run_id: &str) -> bool {
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

    /// The `external_ref` of `chat`'s sessions with the agent it talks to
    /// (`/agent` in Telegram; agent profiles §6.4).
    fn chat_key(&self, source: &str, chat: &str) -> (String, String) {
        let agent = tern_gateway::chat_agents::get(&self.rt.store, source, chat);
        let key = tern_gateway::chat_agents::session_ref(chat, &agent);
        (agent, key)
    }

    /// The chat's current session: the default agent's in Home (as before
    /// profiles), another agent's in its DM.
    fn chat_session(&self, source: &str, chat: &str) -> anyhow::Result<tern_store::SessionRow> {
        let (agent, key) = self.chat_key(source, chat);
        if let Some(s) = self.rt.store.latest_session_for(source, &key)? {
            return Ok(s);
        }
        if agent != tern_store::DEFAULT_AGENT
            && let Some(dm) = self.rt.store.agent_channel(&agent)?
        {
            return Ok(self.rt.store.create_thread(&tern_store::NewThread {
                id: &tern_core::agent::new_id(),
                source,
                channel_id: &dm.id,
                external_ref: Some(&key),
                ..tern_store::NewThread::default()
            })?);
        }
        Ok(self.rt.store.create_session_with_ref(
            &tern_core::agent::new_id(),
            source,
            &key,
            None,
        )?)
    }
}

/// The manager as the sub-agent host's driver: children are ordinary
/// daemon runs, so they show in `/v1/runs/active` and obey cancel.
pub struct ManagerDriver(pub Weak<Manager>);

impl crate::subagents::RunDriver for ManagerDriver {
    fn drive(&self, run_id: &str, resume: bool) -> bool {
        let Some(m) = self.0.upgrade() else {
            return false;
        };
        match m.rt.store.run(run_id) {
            Ok(Some(run)) => m.spawn(&run.session_id, run_id, resume),
            _ => false,
        }
    }

    fn cancel(&self, run_id: &str) -> bool {
        self.0.upgrade().is_some_and(|m| m.cancel_active(run_id))
    }

    fn is_active(&self, run_id: &str) -> bool {
        self.0.upgrade().is_some_and(|m| m.is_active(run_id))
    }
}

fn err(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

/// The manager as gateways see it.
pub struct Chats(pub Arc<Manager>);

#[async_trait]
impl ChatHost for Chats {
    async fn pending_outbox(&self) -> Vec<tern_core::outbox::OutboxItem> {
        let mut rows = self
            .0
            .rt
            .store
            .outbox(Some("pending"), 200)
            .unwrap_or_default();
        rows.reverse();
        rows.into_iter().map(|r| self.0.outbox_view(r)).collect()
    }

    /// A chat button: the chat is remote, so local-only items are refused.
    async fn decide_outbox(&self, id: i64, approve: bool) -> Result<String, String> {
        if approve {
            self.0
                .approve_outbox(id, None, true)
                .map(|_| "Approved; running it".to_owned())
        } else {
            self.0
                .reject_outbox(id, None, true)
                .map(|_| "Rejected".to_owned())
        }
        .map_err(|e| e.to_string())
    }

    /// A chat posting in topic channel `channel` (C5): the same send as
    /// the app's, with the first answer delivered back to the chat.
    async fn send_to_channel(
        &self,
        source: &str,
        chat: &str,
        channel: &str,
        text: &str,
    ) -> Result<(String, String), String> {
        let store = &self.0.rt.store;
        let c = store
            .channel(channel)
            .map_err(err)?
            .filter(|c| c.kind == "topic" && c.archived_at.is_none())
            .ok_or_else(|| format!("no channel {channel}"))?;
        let target = format!("{source}:{chat}");
        let sent = self
            .0
            .send_to_topic(&c, text, None, None, Some(&target))
            .await
            .map_err(err)?;
        let agent = sent
            .woke
            .first()
            .and_then(|w| store.agent(&w.agent_id).ok().flatten())
            .map_or_else(|| tern_core::channels::author::AGENT.to_owned(), |a| a.name);
        Ok((sent.run_id, agent))
    }

    fn changes(&self) -> Option<tokio::sync::broadcast::Receiver<tern_core::models::Change>> {
        Some(self.0.rt.subscribe_changes())
    }

    /// The chat is a stream like Home (docs/channels-design.md §2.6): a
    /// message goes to the run working there, else to the current segment
    /// or, after idle hours or a new day, to a new one.
    async fn send(&self, source: &str, chat: &str, text: &str) -> Result<String, String> {
        let session = self.0.chat_session(source, chat).map_err(err)?;
        if let Some((run_id, _)) = self.0.queue_message(&session.id, text).map_err(err)? {
            return Ok(run_id);
        }
        let session = self.0.cut(&session, false).map_err(err)?.unwrap_or(session);
        let target = format!("{source}:{chat}");
        let started = self
            .0
            .start(Some(&session.id), None, text, source, "user", Some(&target))
            .await
            .map_err(err)?;
        Ok(started.run_id)
    }

    /// A stranger in a gateway chat (`strangers = "taint"`): a tainted run
    /// in the chat's guest session, never the owner's, with the message
    /// wrapped as untrusted. Its answer goes back to the chat.
    async fn send_untrusted(
        &self,
        source: &str,
        chat: &str,
        text: &str,
        taint: &str,
    ) -> Result<String, String> {
        let key = format!("{chat}|untrusted");
        let store = &self.0.rt.store;
        let session = match store.latest_session_for(source, &key).map_err(err)? {
            Some(s) => s,
            None => store
                .create_session_with_ref(&tern_core::agent::new_id(), source, &key, None)
                .map_err(err)?,
        };
        let input = format!(
            "A {taint} (not the owner of this chat):\n{}",
            tern_core::agent::taint::wrap(&format!("{source}:{chat}"), text)
        );
        let target = format!("{source}:{chat}");
        let started = self
            .0
            .start_tainted(
                Some(&session.id),
                None,
                &input,
                source,
                "user",
                Some(&target),
                Some(taint),
            )
            .await
            .map_err(err)?;
        Ok(started.run_id)
    }

    /// `/new`: New topic. The run in progress is stopped (its answer would
    /// land after "Started a new conversation."), and the next message
    /// starts a segment that carries this one's summary over.
    async fn new_session(&self, source: &str, chat: &str) -> Result<(), String> {
        let old = self.0.chat_session(source, chat).map_err(err)?;
        self.0.stop_session(&old.id, true).await;
        self.0.cut(&old, true).map(|_| ()).map_err(err)
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
        let (_, key) = self.0.chat_key(source, chat);
        match self.0.rt.store.latest_session_for(source, &key) {
            Ok(Some(s)) => self.0.stop_session(&s.id, false).await,
            _ => false,
        }
    }

    async fn status(&self, source: &str, chat: &str) -> String {
        let (_, key) = self.0.chat_key(source, chat);
        let session = self
            .0
            .rt
            .store
            .latest_session_for(source, &key)
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

#[cfg(test)]
mod approval_card_tests;
