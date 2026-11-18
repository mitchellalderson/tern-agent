//! Proactive help, daemon side (docs/innbot-learnings.md P1, P2, P5, P6):
//! the generic dismissible card, and the features that post one.
//!
//! Every feature here has a switch (`[proactive]`, on by default), a hard cap
//! enforced where the card is made, and no model call unless something
//! changed (P6 asks the owning agent, once a week per routine at most). A
//! card is a `needs_you` post in the owning agent's DM; dismissing it closes
//! the post, accepting it hands the agent its next step as a message from
//! the user. Nothing is ever applied by the host.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/v1/proactive/cards[?state=pending\|accepted\|dismissed]` | `[Card]`, newest first |
//! | POST | `/v1/proactive/cards/{id}/accept` | acts on the card (a message to its agent), closes it |
//! | POST | `/v1/proactive/cards/{id}/dismiss` | closes it |
//! | POST | `/v1/proactive/presence` | the desktop is open: `{brief}` says whether "While you were away" was posted |
//! | GET | `/v1/grants` | the standing "always" grants: `[{tool, pattern, note}]` |
//! | POST | `/v1/grants/revoke` | `{tool, pattern}` |

use std::sync::Arc;

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tern_core::channels::Card;
use tern_core::models::Change;
use tern_core::proactive::{self as p, RunEnd, kind};
use tern_store::{CardRow, HOME, NewCard, NewPost, RunRow, ScheduleRow};

use super::api::{ApiError, ApiResult, Shared, internal, not_found};
use super::channels::{final_text, view};
use super::manager::Manager;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/v1/proactive/cards", get(list))
        .route("/v1/proactive/cards/{id}/accept", post(accept))
        .route("/v1/proactive/cards/{id}/dismiss", post(dismiss))
        .route("/v1/proactive/presence", post(presence))
        .route("/v1/grants", get(grants))
        .route("/v1/grants/revoke", post(revoke))
}

#[derive(Deserialize, Default)]
struct ListQuery {
    state: Option<String>,
}

async fn list(State(m): State<Shared>, Query(q): Query<ListQuery>) -> ApiResult<Vec<Card>> {
    let rows =
        m.rt.store
            .cards(q.state.as_deref().filter(|s| *s != "all"), 100)
            .map_err(internal)?;
    // A card reserved for a diagnosis still being written has no post yet.
    Ok(Json(
        rows.into_iter()
            .filter(|c| c.post_id.is_some())
            .map(Card::from)
            .collect(),
    ))
}

fn pending(m: &Manager, id: i64) -> Result<CardRow, ApiError> {
    let c =
        m.rt.store
            .card(id)
            .map_err(internal)?
            .ok_or_else(|| not_found("card"))?;
    if c.state != "pending" {
        return Err(ApiError(
            StatusCode::CONFLICT,
            format!("this card was already {}", c.state),
        ));
    }
    Ok(c)
}

async fn accept(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<i64>,
) -> ApiResult<serde_json::Value> {
    let c = pending(&m, id)?;
    m.accept_card(&c).await.map_err(internal)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn dismiss(
    State(m): State<Shared>,
    UrlPath(id): UrlPath<i64>,
) -> ApiResult<serde_json::Value> {
    let c = pending(&m, id)?;
    m.finish_card(&c, "dismissed").map_err(internal)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Serialize)]
struct Presence {
    /// "While you were away" was just posted.
    brief: bool,
}

async fn presence(State(m): State<Shared>) -> ApiResult<Presence> {
    let brief = m.presence().map_err(internal)?;
    Ok(Json(Presence { brief }))
}

#[derive(Serialize)]
struct Grant {
    tool: String,
    pattern: String,
    /// What the grant covers, in words.
    note: String,
}

async fn grants(State(m): State<Shared>) -> ApiResult<Vec<Grant>> {
    let all = m.rt.store.allowlist_all().map_err(internal)?;
    Ok(Json(
        all.into_iter()
            .map(|(tool, pattern)| Grant {
                note: crate::grants_cmd::describe(&pattern),
                tool,
                pattern,
            })
            .collect(),
    ))
}

#[derive(Deserialize)]
struct RevokeBody {
    tool: String,
    pattern: String,
}

async fn revoke(
    State(m): State<Shared>,
    Json(b): Json<RevokeBody>,
) -> ApiResult<serde_json::Value> {
    if !m
        .rt
        .store
        .remove_allowlist(&b.tool, &b.pattern)
        .map_err(internal)?
    {
        return Err(not_found("grant"));
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

fn cut(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

impl Manager {
    // ---- the generic card ----

    /// Record `card` and post it in its agent's DM, unless `max` cards of
    /// its kind (and subject, when `per_subject`) were made since `since`.
    pub fn post_card(
        &self,
        card: &NewCard<'_>,
        since: i64,
        max: u32,
        per_subject: bool,
    ) -> anyhow::Result<Option<CardRow>> {
        let Some(row) = self
            .rt
            .store
            .create_card_capped(card, since, max, per_subject)?
        else {
            return Ok(None);
        };
        self.publish_card(&row)?;
        Ok(self.rt.store.card(row.id)?)
    }

    /// Write the card's post: a `needs_you` in the agent's DM (Home pointers
    /// for other agents come with it).
    fn publish_card(&self, card: &CardRow) -> anyhow::Result<()> {
        let store = &self.rt.store;
        let Some(channel) = store.agent_channel(&card.agent_id)? else {
            // An agent without a DM (archived): the card can't be seen.
            store.decide_card(card.id, "dismissed")?;
            return Ok(());
        };
        let agent = store.agent(&card.agent_id)?;
        let (author_id, author) = agent
            .map(|a| (a.id, a.name))
            .unwrap_or_else(|| ("tern".into(), "tern".into()));
        let text = if card.body.is_empty() {
            card.title.clone()
        } else {
            format!("{}\n{}", card.title, card.body)
        };
        let (post, _) = store.add_post(&NewPost {
            channel_id: &channel.id,
            author_kind: "agent",
            author_id: &author_id,
            author: &author,
            kind: "needs_you",
            text: &text,
            dedupe_key: Some(&format!("card:{}", card.id)),
            ..NewPost::default()
        })?;
        store.set_card_post(card.id, post.id)?;
        let pointer = store.pointer_to(post.id).ok().flatten();
        self.publish(view(store, post));
        if let Some(pointer) = pointer {
            self.publish(view(store, pointer));
        }
        Ok(())
    }

    /// Close a pending card as `state` and its post with it.
    pub fn finish_card(&self, card: &CardRow, state: &str) -> anyhow::Result<()> {
        let store = &self.rt.store;
        if !store.decide_card(card.id, state)? {
            return Ok(());
        }
        let Some(post) = card.post_id else {
            return Ok(());
        };
        store.resolve_post(post)?;
        let pointer = store.post_by_key(&format!("pointer:{post}"))?;
        for row in [store.post(post)?, pointer].into_iter().flatten() {
            let _ = self.rt.changes.send(Change::PostUpdated {
                post: Box::new(view(store, row)),
            });
        }
        Ok(())
    }

    /// Accept: tell the agent, as the user, what the card proposed.
    pub async fn accept_card(self: &Arc<Self>, card: &CardRow) -> anyhow::Result<()> {
        self.finish_card(card, "accepted")?;
        let Some(message) = card.accept_message.as_deref() else {
            return Ok(());
        };
        let channel = self
            .rt
            .store
            .agent_channel(&card.agent_id)?
            .ok_or_else(|| anyhow::anyhow!("no DM for {}", card.agent_id))?;
        self.send_to_main(&channel, message).await
    }

    // ---- P5: daily heartbeat ----

    /// Once a day per agent, look for things that need the user: routines
    /// paused or failing, connections that need signing in again, waits the
    /// poller missed. Host checks only. Nothing found: no card, no model.
    /// Called every scheduler tick; cheap when nothing is due.
    pub fn heartbeat(&self) {
        if let Err(e) = self.heartbeat_inner() {
            tracing::warn!(error = %e, "heartbeat failed");
        }
    }

    fn heartbeat_inner(&self) -> anyhow::Result<()> {
        let live = self.rt.live();
        if !live.proactive.heartbeat {
            return Ok(());
        }
        let now = chrono::Local::now();
        // Not in quiet hours: a card is a post, but it wakes the app.
        if live.schedules.quiet_hours.release_at(now).is_some() {
            return Ok(());
        }
        let store = &self.rt.store;
        let now_ms = now.timestamp_millis();
        for agent in store.agents(false)? {
            let key = format!("heartbeat:{}", agent.id);
            let last: i64 = store
                .proactive_state(&key)?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if now_ms - last < p::HEARTBEAT_EVERY_MS {
                continue;
            }
            // Mark the check first: one a day, found or not.
            store.set_proactive_state(&key, &now_ms.to_string())?;
            let findings = self.heartbeat_findings(&agent, now_ms)?;
            if findings.is_empty() {
                continue;
            }
            // The same news twice in a week isn't news.
            let fp = findings.join("\n");
            let fp_key = format!("heartbeat_fp:{}", agent.id);
            let seen = store.proactive_state(&fp_key)?;
            if seen.as_deref() == Some(fp.as_str())
                && store.cards_since(kind::HEARTBEAT, Some(&agent.id), now_ms - 7 * p::DAY)? > 0
            {
                continue;
            }
            let body = p::heartbeat_body(&findings);
            let made = self.post_card(
                &NewCard {
                    kind: kind::HEARTBEAT,
                    subject: &agent.id,
                    agent_id: &agent.id,
                    title: "A few things need a look",
                    body: &body,
                    ..NewCard::default()
                },
                now_ms - p::HEARTBEAT_EVERY_MS,
                1,
                true,
            )?;
            if let Some(card) = made {
                store.set_proactive_state(&fp_key, &fp)?;
                self.notify_card(&card);
            }
        }
        Ok(())
    }

    /// What needs `agent`'s owner's attention, one short line each.
    fn heartbeat_findings(
        &self,
        agent: &tern_store::AgentRow,
        now_ms: i64,
    ) -> anyhow::Result<Vec<String>> {
        let store = &self.rt.store;
        let mut out = Vec::new();
        for s in store.schedules()? {
            if s.agent() != agent.id || !s.enabled {
                continue;
            }
            let name = super::scheduler::title(&s);
            if s.paused_until.is_some_and(|u| u > now_ms) {
                let why = s.paused_reason.as_deref().unwrap_or("it hit a limit");
                out.push(format!("Routine “{name}” is paused: {why}."));
            } else if let Some(t) = p::routine_trouble(&self.routine_ends(&s)?) {
                out.push(format!("Routine “{name}” {}.", t.say()));
            }
        }
        // Sign-ins belong to the owner, not an agent: the default agent says so.
        if agent.is_default() {
            for c in store.all_connections()? {
                if c.status != "ok" {
                    let what = c.label.clone().unwrap_or_else(|| c.target.clone());
                    let how = match c.status.as_str() {
                        "needs_reauth" => "needs signing in again",
                        _ => "isn't working",
                    };
                    out.push(format!("{what} {how}."));
                }
            }
        }
        for w in store.waits(Some(&agent.id))? {
            if w.next_check_at < now_ms - p::WAIT_OVERDUE_MS || w.expires_at < now_ms {
                out.push(format!("Waiting on “{}” is overdue.", cut(&w.note, 60)));
            }
        }
        Ok(out)
    }

    // ---- P6 and P1: after a run ----

    /// Follow-ups once a run has finished: a routine in trouble (P6), the
    /// end of a fix investigation, or a repeated ask (P1).
    pub fn after_run(self: &Arc<Self>, run_id: &str) -> anyhow::Result<()> {
        let Some(run) = self.rt.store.run(run_id)? else {
            return Ok(());
        };
        if run.parent_run_id.is_some() {
            return Ok(());
        }
        match run.trigger.as_str() {
            "cron" => self.consider_fix(&run),
            "proactive" => self.finish_fix(&run),
            "user" if run.status == "completed" => {
                // Asks the decision model, so off this task.
                let me = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = me.consider_suggestion(&run).await {
                        tracing::warn!(error = %e, "routine suggestion failed");
                    }
                });
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// How a routine's recent runs ended, oldest first.
    fn routine_ends(&self, s: &ScheduleRow) -> anyhow::Result<Vec<RunEnd>> {
        let store = &self.rt.store;
        let mut runs: Vec<RunRow> = store
            .runs_for_session(&s.session_id)?
            .into_iter()
            .filter(|r| {
                r.trigger == "cron"
                    && r.parent_run_id.is_none()
                    && matches!(r.status.as_str(), "completed" | "failed")
            })
            .collect();
        let skip = runs.len().saturating_sub(5);
        runs.drain(..skip);
        runs.iter()
            .map(|r| {
                Ok(if r.status == "failed" {
                    RunEnd::Failed
                } else if r.quiet || final_text(store, r)?.trim().is_empty() {
                    RunEnd::Empty
                } else {
                    RunEnd::Reported
                })
            })
            .collect()
    }

    /// P6: two failures or three empty reports in a row: the owning agent
    /// looks at the runs and proposes a fix as a card. One per routine per
    /// week, three a day; the agent can read, not change.
    fn consider_fix(self: &Arc<Self>, run: &RunRow) -> anyhow::Result<()> {
        if !self.rt.live().proactive.fix_routines {
            return Ok(());
        }
        let store = &self.rt.store;
        let Some(s) = store.schedule_for_session(&run.session_id)? else {
            return Ok(());
        };
        let Some(trouble) = p::routine_trouble(&self.routine_ends(&s)?) else {
            return Ok(());
        };
        let now = tern_store::now_ms();
        if store.cards_since(kind::ROUTINE_FIX, None, now - p::DAY)? >= p::FIX_PER_DAY {
            return Ok(());
        }
        let name = super::scheduler::title(&s);
        let title = format!("Routine “{name}” {}", trouble.say());
        // Reserve the weekly slot now; the card shows when the diagnosis is in.
        let Some(card) = store.create_card_capped(
            &NewCard {
                kind: kind::ROUTINE_FIX,
                subject: &s.id,
                agent_id: s.agent(),
                title: &title,
                ..NewCard::default()
            },
            now - p::FIX_WINDOW_MS,
            1,
            true,
        )?
        else {
            return Ok(());
        };
        match self.start_fix_run(&s, &name, trouble) {
            Ok(run_id) => {
                store.set_proactive_state(&format!("fixrun:{run_id}"), &card.id.to_string())?;
            }
            Err(e) => {
                tracing::warn!(schedule = %s.id, error = %e, "could not start the fix investigation");
                store.decide_card(card.id, "dismissed")?;
            }
        }
        Ok(())
    }

    /// The investigation: a read-only run by the routine's agent, in a
    /// session of its own, given the last runs as quoted data.
    fn start_fix_run(
        self: &Arc<Self>,
        s: &ScheduleRow,
        name: &str,
        trouble: p::Trouble,
    ) -> anyhow::Result<String> {
        let store = &self.rt.store;
        let mut evidence = String::new();
        let runs = store.runs_for_session(&s.session_id)?;
        for r in runs
            .iter()
            .rev()
            .filter(|r| r.trigger == "cron" && r.parent_run_id.is_none())
            .take(3)
        {
            let said = match r.status.as_str() {
                "failed" => r.error.clone().unwrap_or_else(|| "failed".into()),
                _ => cut(&final_text(store, r)?, 600),
            };
            evidence.push_str(&format!("- {} ({}): {}\n", r.status, r.id, said));
        }
        let prompt = format!(
            "{}\n\nIts prompt: {}\n\nThe last runs:\n{}",
            p::fix_prompt(name, &s.id, trouble),
            cut(&s.prompt, 800),
            tern_core::agent::taint::wrap("routine runs", &evidence)
        );
        let session = store.create_session(&tern_core::agent::new_id(), "proactive", None)?;
        store.set_session_agent(&session.id, s.agent())?;
        let started = self.rt.start_run_tainted(
            Some(&session.id),
            None,
            &prompt,
            "proactive",
            "proactive",
            Some("quoted output of a routine's earlier runs"),
        )?;
        // Reads only, whatever the agent's own level.
        store.set_run_autonomy(&started.run_id, "readonly")?;
        self.spawn(&started.session_id, &started.run_id, false);
        Ok(started.run_id)
    }

    /// The diagnosis is in: fill the reserved card and show it.
    fn finish_fix(&self, run: &RunRow) -> anyhow::Result<()> {
        let store = &self.rt.store;
        let Some(id) = store
            .proactive_state(&format!("fixrun:{}", run.id))?
            .and_then(|v| v.parse::<i64>().ok())
        else {
            return Ok(());
        };
        let Some(card) = store.card(id)? else {
            return Ok(());
        };
        let text = if run.status == "completed" {
            final_text(store, run)?
        } else {
            String::new()
        };
        if text.trim().is_empty() {
            store.decide_card(id, "dismissed")?;
            return Ok(());
        }
        let message = format!(
            "Go ahead and apply the fix you proposed for “{}”:\n{}",
            card.title,
            cut(&text, 1500)
        );
        store.fill_card(
            id,
            &card.title,
            &cut(&text, 700),
            Some("Apply fix"),
            Some(&message),
        )?;
        if let Some(row) = store.card(id)? {
            self.publish_card(&row)?;
            self.notify_card(&row);
        }
        Ok(())
    }

    /// Ask the app to notify about a card that needs a look (the heartbeat,
    /// a routine fix): the same `Change::Notification` local delivery uses.
    /// The brief and suggestions wait quietly to be read.
    fn notify_card(&self, card: &CardRow) {
        let Ok(Some(channel)) = self.rt.store.agent_channel(&card.agent_id) else {
            return;
        };
        let _ = self.rt.changes.send(Change::Notification {
            title: card.title.clone(),
            body: cut(&card.body, 240),
            channel_id: Some(channel.id),
            run_id: None,
        });
    }

    /// P1: the same kind of ask three times in 14 days: suggest a routine.
    /// Host-side word overlap finds them; only then one yes/no through
    /// `decide()` asks whether they are one repeatable job (unanswered: the
    /// host's count stands). The cheap caps come first, so the model is asked
    /// at most when a card is about to be made.
    async fn consider_suggestion(&self, run: &RunRow) -> anyhow::Result<()> {
        if !self.rt.live().proactive.suggest_routines || run.taint.is_some() {
            return Ok(());
        }
        let store = &self.rt.store;
        let Some(ask) = store.run_input_text(&run.id)? else {
            return Ok(());
        };
        if !p::countable_ask(&ask) || ask.chars().count() > 600 {
            return Ok(());
        }
        let now = tern_store::now_ms();
        let agent = store.run_agent(&run.id)?.0;
        let earlier = store.recent_user_asks(&agent, now - p::SUGGEST_WINDOW_MS, &run.id, 200)?;
        if p::similar_asks(&ask, &earlier) < p::SUGGEST_ASKS {
            return Ok(());
        }
        // Already a routine for it, or told about it lately: stay quiet.
        let mine = p::tokens(&ask);
        if store
            .schedules()?
            .iter()
            .any(|s| p::alike(&mine, &p::tokens(&s.prompt)))
        {
            return Ok(());
        }
        let sig = format!("suggest:{}", p::ask_signature(&ask));
        if let Some(at) = store
            .proactive_state(&sig)?
            .and_then(|v| v.parse::<i64>().ok())
            && now - at < p::SUGGEST_WINDOW_MS
        {
            return Ok(());
        }
        if store.cards_since(kind::ROUTINE_SUGGESTION, None, now - p::DAY)? >= p::SUGGEST_PER_DAY {
            return Ok(());
        }
        let alike_asks: Vec<&String> = std::iter::once(&ask)
            .chain(earlier.iter().filter(|e| p::alike(&mine, &p::tokens(e))))
            .take(5)
            .collect();
        let state = format!(
            "The user asked for the same kind of work several times:\n{}",
            alike_asks
                .iter()
                .map(|a| format!("- {}", cut(a, 160)))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let answers = self
            .rt
            .decide(
                &state,
                vec![(
                    "repeat".into(),
                    tern_core::decide::Question::yes_no(p::REPEAT_QUESTION),
                )],
            )
            .await;
        if answers["repeat"].is_yes(p::REPEAT_THRESHOLD) == Some(false) {
            return Ok(());
        }
        let title = "Make this a routine?";
        let body = format!(
            "You've asked for something like “{}” a few times lately.",
            cut(&ask, 80)
        );
        let message = format!(
            "Set this up as a routine: “{}”. Ask me when it should run.",
            cut(&ask, 400)
        );
        let made = self.post_card(
            &NewCard {
                kind: kind::ROUTINE_SUGGESTION,
                subject: &sig,
                agent_id: &agent,
                title,
                body: &body,
                accept_label: Some("Set up routine"),
                accept_message: Some(&message),
            },
            now - p::DAY,
            p::SUGGEST_PER_DAY,
            false,
        )?;
        if made.is_some() {
            store.set_proactive_state(&sig, &now.to_string())?;
        }
        Ok(())
    }

    // ---- P2: while you were away ----

    /// The desktop reports it is open. On the first open of the day after
    /// eight hours or more away, post one brief in Home: what the digest
    /// held, what waits for the user. Nothing to say: no card (and no model
    /// either way). True if a brief was posted.
    pub fn presence(&self) -> anyhow::Result<bool> {
        let store = &self.rt.store;
        let now = tern_store::now_ms();
        let prev: Option<i64> = store
            .proactive_state("presence:last_seen")?
            .and_then(|v| v.parse().ok());
        store.set_proactive_state("presence:last_seen", &now.to_string())?;
        let (Some(prev), true) = (prev, self.rt.live().proactive.away_brief) else {
            return Ok(false);
        };
        let today = today();
        let last_day = store.proactive_state("away:day")?;
        if !p::away_due(now - prev, last_day.as_deref(), &today) {
            return Ok(false);
        }
        // Today's chance is used up whether or not there's news.
        store.set_proactive_state("away:day", &today)?;
        let held = store.held_deliveries_of_kind("digest")?;
        let mut lines = Vec::new();
        let outbox = store.outbox(Some("pending"), 200)?.len();
        if outbox > 0 {
            lines.push(format!(
                "{outbox} {} waiting in the outbox.",
                if outbox == 1 { "item" } else { "items" }
            ));
        }
        let open = store.open_needs_you(HOME)?;
        if open > 0 {
            lines.push(format!("{open} open in Home."));
        }
        let reports = store
            .posts_between(HOME, prev, now)?
            .iter()
            .filter(|p| p.author_kind == "routine")
            .count();
        if reports > 0 {
            lines.push(format!(
                "{reports} routine {} came in.",
                if reports == 1 { "report" } else { "reports" }
            ));
        }
        if held.is_empty() && lines.is_empty() {
            return Ok(false);
        }
        let mut body = lines.join("\n");
        if !held.is_empty() {
            let items: Vec<&tern_store::HeldDelivery> = held.iter().collect();
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            body.push_str(&cut(&super::scheduler::combine(&items), 3000));
        }
        let made = self.post_card(
            &NewCard {
                kind: kind::AWAY_BRIEF,
                subject: &today,
                agent_id: tern_store::DEFAULT_AGENT,
                title: "While you were away",
                body: &body,
                ..NewCard::default()
            },
            now - p::DAY,
            1,
            false,
        )?;
        if made.is_some() {
            // It's in the brief now; the digest time has nothing left to send.
            let ids: Vec<i64> = held.iter().map(|h| h.id).collect();
            store.delete_held_deliveries(&ids)?;
        }
        Ok(made.is_some())
    }
}
