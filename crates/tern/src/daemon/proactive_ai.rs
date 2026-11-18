//! Proactive help that asks a model, a little (docs/innbot-learnings.md P3,
//! P4, P9, P11).
//!
//! Every pass here follows the same shape: a switch (`[proactive]`, on by
//! default), a hard cap checked before anything is asked, a cheap `decide()`
//! yes/no first, and only then one fast-model call that writes. When the
//! decision is "no" or unanswered, nothing else happens: a refusal is never
//! read as "yes". Nothing runs for a tainted run. The user sees each result
//! as a dismissible card, and every change is reversible from it.
//!
//! - P3 **New topic?**: a hint in Home or a DM when the subject clearly
//!   changes. It never switches by itself; accepting cuts the stream.
//! - P4 **open loops**: "I'll check back Friday" in a reply becomes a
//!   follow-up on the `wait_until` machinery (a time wait up to 30 days).
//! - P11 **proactive memory**: durable facts in a user message the agent
//!   didn't save, up to three, deduplicated against existing memory.
//! - P9 **weekly tidy-up**: duplicate memories merged and contradictions
//!   flagged, logged with undo (`GET /v1/memory-tidy`).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::Local;
use serde::Serialize;
use tern_core::decide::Question;
use tern_core::proactive::{self as p, kind};
use tern_store::{
    CardRow, ChannelRow, DEFAULT_AGENT, NewCard, NewMemory, NewWait, RunRow, SOURCE_AGENT,
    SOURCE_UNVERIFIED, SOURCE_USER, Undo,
};
use tern_tools::wait::Check;

use super::channels::final_text;
use super::manager::Manager;

const WRITE_TIMEOUT: Duration = Duration::from_secs(20);

fn trust(source: &str) -> u8 {
    match source {
        SOURCE_UNVERIFIED => 0,
        SOURCE_USER => 2,
        _ => 1,
    }
}

fn cut(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flat.chars().take(max).collect();
    if flat.chars().count() > max {
        out.push('…');
    }
    out
}

impl Manager {
    // ---- after a run: P11 and P4 ----

    /// Model passes for a finished run: facts from the user's message (P11)
    /// and promises in the reply (P4). Spawned: the run's own end never
    /// waits for them. A tainted run is never read.
    pub fn after_run_ai(self: &Arc<Self>, run_id: &str) {
        let live = self.rt.live().proactive;
        if !live.learn_facts && !live.open_loops {
            return;
        }
        let (me, id) = (self.clone(), run_id.to_owned());
        tokio::spawn(async move {
            if let Err(e) = me.run_ai(&id).await {
                tracing::warn!(run = %id, error = %e, "proactive pass failed");
            }
        });
    }

    async fn run_ai(self: &Arc<Self>, run_id: &str) -> anyhow::Result<()> {
        let Some(run) = self.rt.store.run(run_id)? else {
            return Ok(());
        };
        if run.parent_run_id.is_some()
            || run.status != "completed"
            || run.taint.is_some()
            || run.input_taint.is_some()
        {
            return Ok(());
        }
        if run.trigger == "user" {
            self.learn_facts(&run).await?;
        }
        self.open_loops(&run).await
    }

    // ---- P11: proactive memory ----

    /// "Does this message contain a durable fact?" (`decide()`), then one
    /// fast-model pass that saves up to three the agent missed. Saved through
    /// `Store::remember`, so the memory caps and the run's write limit apply.
    async fn learn_facts(&self, run: &RunRow) -> anyhow::Result<()> {
        if !self.rt.live().proactive.learn_facts {
            return Ok(());
        }
        let store = &self.rt.store;
        let Some(text) = store.run_input_text(&run.id)? else {
            return Ok(());
        };
        let text = text.trim();
        let chars = text.chars().count();
        if !(p::FACT_MIN_CHARS..=p::FACT_MAX_CHARS).contains(&chars) || text.starts_with('/') {
            return Ok(());
        }
        let now = tern_store::now_ms();
        if store.cards_since(kind::MEMORY_LEARNED, None, now - p::DAY)? >= p::FACT_CARDS_PER_DAY {
            return Ok(());
        }
        let answers = self
            .rt
            .decide(
                text,
                vec![("fact".into(), Question::yes_no(p::FACT_QUESTION))],
            )
            .await;
        if answers["fact"].is_yes(p::FACT_THRESHOLD) != Some(true) {
            return Ok(());
        }
        let (agent, private) = store.run_agent(&run.id)?;
        let readonly = run.autonomy.as_deref() == Some("readonly");
        let ns = (agent != DEFAULT_AGENT && (private || readonly)).then(|| agent.clone());
        let mut known: Vec<String> = store
            .memories_in(ns.as_deref())?
            .into_iter()
            .map(|m| m.content)
            .collect();
        known.reverse(); // newest first: what the writer is shown
        let said = self
            .rt
            .fast_text(
                p::FACT_SYSTEM,
                p::facts_prompt(text, &known),
                300,
                WRITE_TIMEOUT,
            )
            .await?;
        let facts = p::new_facts(&said, &known);
        let limits = self.rt.live().memory.limits();
        let tags = ["auto".to_owned()];
        let mut saved: Vec<(i64, String)> = Vec::new();
        for fact in facts {
            let made = store.remember(
                &NewMemory {
                    content: &fact,
                    tags: &tags,
                    agent: ns.as_deref(),
                    source: SOURCE_AGENT,
                    run_id: &run.id,
                    session_id: &run.session_id,
                    actor_agent: &agent,
                },
                &limits,
            )?;
            match made {
                Ok(id) => saved.push((id, fact)),
                // A cap says stop; it is not retried with the next fact.
                Err(_) => break,
            }
        }
        if saved.is_empty() {
            return Ok(());
        }
        let ids = saved
            .iter()
            .map(|(id, _)| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let body = saved
            .iter()
            .map(|(_, f)| format!("• {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        self.post_card(
            &NewCard {
                kind: kind::MEMORY_LEARNED,
                subject: &ids,
                agent_id: &agent,
                title: "Remembered",
                body: &body,
                accept_label: Some("Forget"),
                ..NewCard::default()
            },
            now - p::DAY,
            p::FACT_CARDS_PER_DAY,
            false,
        )?;
        Ok(())
    }

    /// "Forget" on a card: delete what it saved. Recorded as the owner's.
    pub(super) fn forget_learned(&self, card: &CardRow) -> anyhow::Result<()> {
        for id in card
            .subject
            .split(',')
            .filter_map(|i| i.parse::<i64>().ok())
        {
            self.rt.store.delete_memory(id)?;
        }
        Ok(())
    }

    // ---- P4: open loops ----

    /// "Does the reply promise to check back?" (`decide()`), then one
    /// fast-model extraction with a date, then a time wait on the agent's
    /// conversation. The agent wakes by itself when it is due.
    async fn open_loops(&self, run: &RunRow) -> anyhow::Result<()> {
        let live = self.rt.live();
        if !live.proactive.open_loops {
            return Ok(());
        }
        let store = &self.rt.store;
        let reply = final_text(store, run)?;
        let reply = reply.trim();
        if reply.chars().count() < 20 {
            return Ok(());
        }
        let (agent, _) = store.run_agent(&run.id)?;
        // The agent already set a wait this run: nothing to add.
        if store
            .waits(Some(&agent))?
            .iter()
            .any(|w| w.run_id == run.id)
        {
            return Ok(());
        }
        let now_ms = tern_store::now_ms();
        if store.followups_open(&agent)? >= p::FOLLOWUPS_OPEN
            || store.cards_since(kind::FOLLOW_UP, None, now_ms - p::DAY)? >= p::FOLLOWUPS_PER_DAY
        {
            return Ok(());
        }
        let shown: String = reply.chars().take(p::FOLLOWUP_REPLY_CHARS).collect();
        let answers = self
            .rt
            .decide(
                &shown,
                vec![("follow_up".into(), Question::yes_no(p::FOLLOWUP_QUESTION))],
            )
            .await;
        if answers["follow_up"].is_yes(p::FOLLOWUP_THRESHOLD) != Some(true) {
            return Ok(());
        }
        let now = Local::now();
        let said = self
            .rt
            .fast_text(
                p::FOLLOWUP_SYSTEM,
                p::followup_prompt(&shown, now),
                200,
                WRITE_TIMEOUT,
            )
            .await?;
        let Some(f) = p::parse_followup(&said, now) else {
            return Ok(());
        };
        let id = tern_core::agent::new_id();
        let secs = ((f.due_ms - now_ms) / 1000).max(1);
        let deliver_to = tern_tools::schedule::here(store, &run.session_id);
        let spec = Check::Time.spec().to_string();
        let new = NewWait {
            id: &id,
            session_id: &run.session_id,
            run_id: &run.id,
            agent_id: &agent,
            note: &f.note,
            check_kind: Check::Time.kind(),
            check_spec: &spec,
            until_spec: None,
            every_secs: secs,
            next_check_at: f.due_ms,
            expires_at: f.due_ms,
            tainted: false,
            deliver_to: deliver_to.as_deref(),
            followup: true,
        };
        // Follow-ups have room of their own on top of `max_waits`.
        let room = live.schedules.max_waits + p::FOLLOWUPS_OPEN as usize;
        if store.create_wait(&new, room)?.is_none() {
            return Ok(());
        }
        self.post_card(
            &NewCard {
                kind: kind::FOLLOW_UP,
                subject: &id,
                agent_id: &agent,
                title: &format!("I'll follow up {}", p::due_label(f.due_ms)),
                body: &f.note,
                accept_label: Some("Cancel"),
                ..NewCard::default()
            },
            now_ms - p::DAY,
            p::FOLLOWUPS_PER_DAY,
            false,
        )?;
        Ok(())
    }

    /// "Cancel" on a follow-up card: the wait ends, nothing continues.
    pub(super) fn cancel_followup(&self, card: &CardRow) -> anyhow::Result<()> {
        self.rt.store.end_wait(
            &card.subject,
            "wait.cancel",
            tern_store::ACTOR_OWNER,
            "cancelled",
        )?;
        Ok(())
    }

    /// A follow-up ended (fired or cancelled): close its card.
    pub fn followup_ended(&self, wait_id: &str) {
        if let Ok(Some(card)) = self.rt.store.pending_card_for(kind::FOLLOW_UP, wait_id) {
            let _ = self.finish_card(&card, "dismissed");
        }
    }

    // ---- P3: "New topic?" ----

    /// After the user sends `text` in Home or a DM and the run starts: one
    /// `decide()` yes/no on whether the subject clearly changed, only when
    /// the conversation has some history, at most once per conversation.
    /// The hint is a card; accepting it cuts the stream. Spawned.
    pub fn consider_new_topic(
        self: &Arc<Self>,
        channel: &ChannelRow,
        session_id: &str,
        run_id: &str,
        text: &str,
    ) {
        if !self.rt.live().proactive.new_topic {
            return;
        }
        let (me, channel) = (self.clone(), channel.clone());
        let (session, run, text) = (session_id.to_owned(), run_id.to_owned(), text.to_owned());
        tokio::spawn(async move {
            if let Err(e) = me.new_topic_hint(&channel, &session, &run, &text).await {
                tracing::warn!(error = %e, "new-topic hint failed");
            }
        });
    }

    async fn new_topic_hint(
        &self,
        channel: &ChannelRow,
        session: &str,
        run_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let store = &self.rt.store;
        if !p::countable_ask(text) {
            return Ok(());
        }
        let now = tern_store::now_ms();
        // Caps first: no model call once either is spent.
        if store.cards_since(kind::NEW_TOPIC, Some(session), 0)? >= p::HINTS_PER_SESSION
            || store.cards_since(kind::NEW_TOPIC, None, now - p::DAY)? >= p::HINTS_PER_DAY
        {
            return Ok(());
        }
        let mut exchanges: Vec<(String, String)> = Vec::new();
        for r in store.runs_for_session(session)? {
            if r.id == run_id
                || r.trigger != "user"
                || r.parent_run_id.is_some()
                || r.status != "completed"
            {
                continue;
            }
            let you = store.run_input_text(&r.id)?.unwrap_or_default();
            exchanges.push((you, final_text(store, &r)?));
        }
        if exchanges.len() < p::HINT_MIN_USER_MESSAGES {
            return Ok(());
        }
        let answers = self
            .rt
            .decide(
                &p::hint_state(&exchanges, text),
                vec![("new_topic".into(), Question::yes_no(p::HINT_QUESTION))],
            )
            .await;
        if answers["new_topic"].is_yes(p::HINT_THRESHOLD) != Some(true) {
            return Ok(());
        }
        // The user may have started a new topic themselves meanwhile.
        let current = store.channel(&channel.id)?.and_then(|c| c.main_session_id);
        if current.as_deref() != Some(session) {
            return Ok(());
        }
        let agent = channel.agent_id.as_deref().unwrap_or(DEFAULT_AGENT);
        self.post_card(
            &NewCard {
                kind: kind::NEW_TOPIC,
                subject: session,
                agent_id: agent,
                title: "New topic?",
                body: "This looks like a different subject. A fresh start keeps the context clean.",
                accept_label: Some("New topic"),
                channel_id: Some(&channel.id),
                ..NewCard::default()
            },
            now - p::DAY,
            p::HINTS_PER_DAY,
            false,
        )?;
        Ok(())
    }

    /// "New topic" on the hint: cut the stream here, with the usual
    /// carry-over, if it is still the conversation the hint was about.
    pub(super) fn start_new_topic(&self, card: &CardRow) -> anyhow::Result<()> {
        let store = &self.rt.store;
        let Some(channel) = card
            .channel_id
            .as_deref()
            .and_then(|c| store.channel(c).ok().flatten())
        else {
            return Ok(());
        };
        let Some(head) = channel
            .main_session_id
            .as_deref()
            .and_then(|s| store.session(s).ok().flatten())
            .filter(|s| s.id == card.subject)
        else {
            return Ok(());
        };
        if let Some(next) = self.cut(&head, true)? {
            store.set_main_session(&channel.id, &next.id)?;
            self.channels_changed();
        }
        Ok(())
    }

    // ---- P9: weekly memory tidy-up ----

    /// Once a week: merge duplicate memories and flag contradictions. Called
    /// every scheduler tick; cheap when nothing is due. The first check only
    /// starts the week.
    pub fn tidy_tick(self: &Arc<Self>) {
        if let Err(e) = self.tidy_due() {
            tracing::warn!(error = %e, "memory tidy-up check failed");
        }
    }

    fn tidy_due(self: &Arc<Self>) -> anyhow::Result<()> {
        let live = self.rt.live();
        if !live.proactive.tidy_memory {
            return Ok(());
        }
        let now = Local::now();
        if live.schedules.quiet_hours.release_at(now).is_some() {
            return Ok(());
        }
        let store = &self.rt.store;
        let now_ms = now.timestamp_millis();
        let last: Option<i64> = store
            .proactive_state("tidy:last")?
            .and_then(|v| v.parse().ok());
        match last {
            None => store.set_proactive_state("tidy:last", &now_ms.to_string())?,
            Some(l) if now_ms - l >= p::TIDY_EVERY_MS => {
                // Marked first: one pass a week, found anything or not.
                store.set_proactive_state("tidy:last", &now_ms.to_string())?;
                let me = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = me.tidy_memory().await {
                        tracing::warn!(error = %e, "memory tidy-up failed");
                    }
                });
            }
            Some(_) => {}
        }
        Ok(())
    }

    /// One pass over every namespace that changed since the last one.
    pub async fn tidy_memory(&self) -> anyhow::Result<()> {
        let store = &self.rt.store;
        let entry_max = self.rt.live().memory.entry_max_chars;
        let (mut merged, mut flagged) = (0usize, 0usize);
        for ns in store.memory_namespaces()? {
            let label = ns.clone().unwrap_or_else(|| "shared".into());
            let rows = store.memories_in(ns.as_deref())?;
            if rows.len() < p::TIDY_MIN {
                continue;
            }
            // Nothing changed since the last pass: no model call at all.
            let fp_key = format!("tidy_fp:{label}");
            let fp = store.memory_fingerprint(ns.as_deref())?;
            if store.proactive_state(&fp_key)?.as_deref() == Some(fp.as_str()) {
                continue;
            }
            let skip = rows.len().saturating_sub(p::TIDY_MAX_READ);
            let notes: Vec<(i64, String, String)> = rows
                .iter()
                .skip(skip)
                .map(|m| (m.id, m.source.clone(), m.content.clone()))
                .collect();
            let list = p::tidy_list(&notes);
            let known: BTreeSet<i64> = notes.iter().map(|n| n.0).collect();
            let gate = self
                .rt
                .decide(
                    &list,
                    vec![("tidy".into(), Question::yes_no(p::TIDY_QUESTION))],
                )
                .await;
            // "No" ends it; unanswered goes on to the writer.
            if gate["tidy"].is_yes(0.5) != Some(false) {
                let said = self
                    .rt
                    .fast_text(p::TIDY_SYSTEM, list, 1500, Duration::from_secs(40))
                    .await?;
                let (merges, conflicts) = p::parse_tidy(&said, &known);
                for m in merges {
                    if merged >= p::TIDY_MAX_MERGES {
                        break;
                    }
                    let Some(keeper) = m
                        .ids
                        .iter()
                        .filter_map(|i| rows.iter().find(|r| r.id == *i))
                        .max_by_key(|r| (trust(&r.source), std::cmp::Reverse(r.id)))
                    else {
                        continue;
                    };
                    let remove: Vec<i64> =
                        m.ids.iter().copied().filter(|i| *i != keeper.id).collect();
                    let text = Some(m.text.as_str()).filter(|t| t.chars().count() <= entry_max);
                    let note = cut(text.unwrap_or(&keeper.content), 100);
                    if store
                        .tidy_merge(ns.as_deref(), keeper.id, &remove, text, &note)?
                        .is_some()
                    {
                        merged += remove.len();
                    }
                }
                for c in conflicts {
                    if flagged >= p::TIDY_MAX_FLAGS {
                        break;
                    }
                    let note = if c.note.is_empty() {
                        "These disagree.".to_owned()
                    } else {
                        c.note
                    };
                    if store.tidy_flag(ns.as_deref(), &c.ids, &note)?.is_some() {
                        flagged += 1;
                    }
                }
            }
            // After the changes, so the next pass skips what it just left.
            store.set_proactive_state(&fp_key, &store.memory_fingerprint(ns.as_deref())?)?;
        }
        if merged + flagged == 0 {
            return Ok(());
        }
        let mut lines = Vec::new();
        if merged > 0 {
            lines.push(format!(
                "Merged {merged} duplicate {}.",
                if merged == 1 { "memory" } else { "memories" }
            ));
        }
        if flagged > 0 {
            lines.push(format!(
                "{flagged} {} to check.",
                if flagged == 1 {
                    "memory pair disagrees"
                } else {
                    "memory pairs disagree"
                }
            ));
        }
        lines.push("Undo any of it in Memory.".to_owned());
        let now = tern_store::now_ms();
        self.post_card(
            &NewCard {
                kind: kind::MEMORY_TIDY,
                subject: &Local::now().format("%Y-%m-%d").to_string(),
                agent_id: DEFAULT_AGENT,
                title: "Memory tidied",
                body: &lines.join("\n"),
                ..NewCard::default()
            },
            now - p::TIDY_EVERY_MS / 2,
            1,
            false,
        )?;
        Ok(())
    }
}

/// One tidy-up log entry, for the Memory page.
#[derive(Serialize)]
pub struct Tidy {
    pub id: i64,
    pub at: i64,
    /// `merge` or `conflict`.
    pub action: String,
    pub namespace: String,
    pub note: String,
    /// `applied`, `undone` or `dismissed`.
    pub state: String,
    /// The memories involved, as they were: for a merge, the one that
    /// stayed first, then the ones that went.
    pub memories: Vec<TidyMemory>,
}

#[derive(Serialize)]
pub struct TidyMemory {
    pub id: i64,
    pub content: String,
    pub source: String,
}

pub fn tidy_view(t: tern_store::TidyRow) -> Tidy {
    let before: serde_json::Value = serde_json::from_str(&t.before).unwrap_or_default();
    let one = |v: &serde_json::Value| TidyMemory {
        id: v["id"].as_i64().unwrap_or_default(),
        content: v["content"].as_str().unwrap_or_default().to_owned(),
        source: v["source"].as_str().unwrap_or_default().to_owned(),
    };
    let mut memories = Vec::new();
    if t.action == tern_store::TIDY_MERGE {
        memories.push(one(&before["kept"]));
        memories.extend(before["removed"].as_array().into_iter().flatten().map(one));
    } else {
        memories.extend(before["rows"].as_array().into_iter().flatten().map(one));
    }
    Tidy {
        id: t.id,
        at: t.at,
        action: t.action,
        namespace: t.namespace,
        note: t.note,
        state: t.state,
        memories,
    }
}

/// Undo one tidy-up entry (or dismiss a flag).
pub fn tidy_undo(m: &Manager, id: i64) -> anyhow::Result<bool> {
    Ok(m.rt.store.tidy_undo(id)? == Undo::Done)
}
