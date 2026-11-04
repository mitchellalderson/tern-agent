//! Agents in topic channels (docs/agent-profiles-design.md §5): who answers
//! a post, `@mention` wakes, and the guards on agents waking agents.

use std::sync::Arc;

use tern_core::channels::{Post, author};
use tern_core::models::Change;
use tern_store::{AgentRow, ChannelRow, DEFAULT_AGENT, NewPost, NewThread};
use tokio_util::sync::CancellationToken;

use super::channels::{Sent, view};
use super::manager::Manager;

/// A run a post started (or the run it was queued for).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Woken {
    pub agent_id: String,
    pub run_id: String,
    pub session_id: String,
    /// Given to the run already working in the agent's thread.
    pub queued: bool,
}

/// How long the hourly wake budget looks back.
const HOUR_MS: i64 = 60 * 60 * 1000;

impl Manager {
    /// The agent that answers posts mentioning no one: the lead, or the
    /// default agent while the lead is archived.
    fn answering_lead(&self, channel: &ChannelRow) -> anyhow::Result<String> {
        let lead = channel.lead();
        Ok(match self.rt.store.agent(lead)? {
            Some(a) if a.archived_at.is_none() => a.id,
            _ => DEFAULT_AGENT.into(),
        })
    }

    /// `agent`'s main thread in topic channel `channel`, created on first
    /// use (§5.1): the lead's is the channel's main thread.
    pub(super) fn agent_thread(&self, channel: &ChannelRow, agent: &str) -> anyhow::Result<String> {
        if agent == channel.lead() {
            return self.main_thread(channel);
        }
        let store = &self.rt.store;
        if let Some(s) = store.member_thread(&channel.id, agent)? {
            return Ok(s);
        }
        let s = store.create_thread(&NewThread {
            id: &tern_core::agent::new_id(),
            source: "api",
            channel_id: &channel.id,
            agent_id: Some(agent),
            ..NewThread::default()
        })?;
        store.set_member_thread(&channel.id, agent, &s.id)?;
        Ok(s.id)
    }

    /// Who answers a reply to post `to` that mentions no one: the agent
    /// that wrote it (a routine's: its agent), else the lead. The answer
    /// is in that agent's conversation in the channel, like any other.
    fn replier(&self, channel: &ChannelRow, to: i64) -> anyhow::Result<String> {
        let store = &self.rt.store;
        let thread = store
            .post(to)?
            .filter(|p| p.channel_id == channel.id && p.author_kind != "user")
            .and_then(|p| p.session_id);
        match thread {
            Some(s) => {
                let agent = store.session_agent(&s)?;
                Ok(match store.agent(&agent)? {
                    Some(a) if a.archived_at.is_none() => a.id,
                    _ => self.answering_lead(channel)?,
                })
            }
            None => self.answering_lead(channel),
        }
    }

    /// Add `agent` to topic channel `channel`, with a "Research joined"
    /// note in its timeline. True if it wasn't a member.
    pub(super) fn join(&self, channel: &ChannelRow, agent: &AgentRow) -> anyhow::Result<bool> {
        if !self.rt.store.add_channel_agent(channel, &agent.id)? {
            return Ok(false);
        }
        self.note(
            &channel.id,
            &format!("{} joined", agent.name),
            &format!(
                "joined:{}:{}:{}",
                channel.id,
                agent.id,
                tern_store::now_ms()
            ),
        )?;
        self.channels_changed();
        let _ = self.rt.changes.send(Change::AgentsChanged {});
        Ok(true)
    }

    /// A quiet line from tern itself in a channel's timeline, once per
    /// `key`.
    pub(super) fn note(&self, channel_id: &str, text: &str, key: &str) -> anyhow::Result<()> {
        let (row, created) = self.rt.store.add_post(&NewPost {
            channel_id,
            author_kind: "system",
            author_id: author::SYSTEM,
            author: author::AGENT,
            kind: "note",
            text,
            dedupe_key: Some(key),
            ..NewPost::default()
        })?;
        if created {
            self.publish(view(&self.rt.store, row));
        }
        Ok(())
    }

    /// Start `agent`'s run in `session` for post `post_id`, or give the
    /// text to the run already working there. A run a tainted post woke
    /// starts tainted (§6.2); a queued one becomes tainted.
    #[allow(clippy::too_many_arguments)]
    async fn wake(
        self: &Arc<Self>,
        agent: &str,
        session: &str,
        post_id: i64,
        text: &str,
        taint: Option<&str>,
        model: Option<&str>,
        trigger: &str,
    ) -> anyhow::Result<Woken> {
        let store = &self.rt.store;
        if let Some((run_id, _)) = self.queue_message(session, text)? {
            if let Some(t) = taint {
                store.set_run_taint(&run_id, t)?;
            }
            return Ok(Woken {
                agent_id: agent.into(),
                run_id,
                session_id: session.into(),
                queued: true,
            });
        }
        let started = self
            .rt
            .start_run(Some(session), model, text, "api", trigger)?;
        store.set_run_woken(&started.run_id, post_id, taint)?;
        self.spawn(&started.session_id, &started.run_id, false);
        Ok(Woken {
            agent_id: agent.into(),
            run_id: started.run_id,
            session_id: started.session_id,
            queued: false,
        })
    }

    /// The user posts `text` in topic channel `channel` (§5.2): with no
    /// mentions the lead answers (a reply: whoever wrote the post it
    /// replies to); otherwise each agent mentioned does, in parallel, each
    /// in its own conversation there. Mentioning an agent that isn't a
    /// member adds it. The timeline is flat: `reply_to` only quotes.
    pub(super) async fn send_to_topic(
        self: &Arc<Self>,
        channel: &ChannelRow,
        text: &str,
        reply_to: Option<i64>,
        model: Option<&str>,
    ) -> anyhow::Result<Sent> {
        let store = &self.rt.store;
        let mentioned = store.mentioned_agents(text)?;
        for a in &mentioned {
            self.join(channel, a)?;
        }
        let agents: Vec<String> = if mentioned.is_empty() {
            vec![match reply_to {
                Some(to) => self.replier(channel, to)?,
                None => self.answering_lead(channel)?,
            }]
        } else {
            mentioned.into_iter().map(|a| a.id).collect()
        };
        // Each agent answers in its own conversation in the channel, a
        // reply as much as a new post: `reply_to` only quotes.
        let mut threads = Vec::with_capacity(agents.len());
        for agent in agents {
            let session = self.agent_thread(channel, &agent)?;
            threads.push((agent, session));
        }
        // The post first, so each run is started knowing which post woke
        // it (its answer replies to it; its digest leaves it out).
        let key = format!("send:{}", tern_core::agent::new_id());
        let (row, _) = store.add_post(&NewPost {
            channel_id: &channel.id,
            session_id: threads.first().map(|(_, s)| s.as_str()),
            author_kind: "user",
            author_id: author::OWNER,
            author: "you",
            kind: "message",
            text,
            reply_to,
            dedupe_key: Some(&key),
            ..NewPost::default()
        })?;
        let mut woke = Vec::with_capacity(threads.len());
        for (agent, session) in &threads {
            woke.push(
                self.wake(agent, session, row.id, text, None, model, "user")
                    .await?,
            );
        }
        let first = woke
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("nobody to answer"))?;
        store.set_post_run(row.id, &first.session_id, &first.run_id)?;
        let post = store.post(row.id)?.map(|r| view(store, r));
        if let Some(p) = &post {
            self.publish(p.clone());
        }
        Ok(Sent {
            post,
            run_id: first.run_id,
            session_id: first.session_id,
            queued: first.queued,
            new_segment: false,
            woke,
        })
    }

    /// The agent behind an agent's, a sub-agent's or a routine's post.
    fn post_agent(&self, post: &Post) -> anyhow::Result<Option<String>> {
        Ok(match post.author_kind.as_str() {
            "agent" => Some(post.author_id.clone()),
            "subagent" => post.author_id.split('/').next().map(str::to_owned),
            "routine" => match &post.session_id {
                Some(s) => Some(self.rt.store.session_agent(s)?),
                None => None,
            },
            _ => None,
        })
    }

    /// An agent's (or a routine's) post that @mentions agents (§5.2,
    /// §5.3): wake them if the channel allows agent wakes, the post isn't
    /// `max_hops` deep, and the hourly budget isn't used up. One wake per
    /// agent per post; an agent never wakes itself.
    pub(super) async fn wake_from_post(self: &Arc<Self>, post: &Post) -> anyhow::Result<()> {
        if post.mentions.is_empty()
            || !matches!(post.author_kind.as_str(), "agent" | "subagent" | "routine")
            || matches!(post.kind.as_str(), "pointer" | "note" | "moved")
        {
            return Ok(());
        }
        let store = &self.rt.store;
        let Some(channel) = store.channel(&post.channel_id)? else {
            return Ok(());
        };
        if channel.kind != "topic" || channel.archived_at.is_some() {
            return Ok(());
        }
        let wakes = store.channel_wakes(&channel.id)?;
        if !wakes.agent_wakes {
            return Ok(());
        }
        let me = self.post_agent(post)?;
        let mut targets = Vec::new();
        for id in &post.mentions {
            if Some(id) == me.as_ref()
                || !store.is_member(&channel, id)?
                || store.woke(post.id, id)?
                || store
                    .mail_by_key(&format!("wake:{}:{id}", post.id))?
                    .is_some()
            {
                continue;
            }
            if let Some(a) = store.agent(id)?.filter(|a| a.archived_at.is_none()) {
                targets.push(a);
            }
        }
        if targets.is_empty() {
            return Ok(());
        }
        let names = |ts: &[AgentRow]| {
            ts.iter()
                .map(|a| format!("@{}", a.slug))
                .collect::<Vec<_>>()
                .join(", ")
        };
        if post.wake_depth >= wakes.max_hops {
            tracing::info!(
                post = post.id,
                depth = post.wake_depth,
                "mention not woken: hop limit"
            );
            return self.note(
                &channel.id,
                &format!(
                    "{} not woken: agents have already woken each other {} times in a row here, the most #{} allows",
                    names(&targets),
                    post.wake_depth,
                    channel.slug
                ),
                &format!("nowake:{}", post.id),
            );
        }
        let body = if post.tainted {
            tern_core::agent::taint::wrap(&format!("post by {}", post.author), &post.text)
        } else {
            post.text.clone()
        };
        let text = format!(
            "{} mentioned you in #{} (post {}):\n\n{body}",
            post.author, channel.slug, post.id
        );
        let taint = post
            .tainted
            .then(|| format!("woken by a tainted post from {}", post.author));
        for agent in targets {
            let used = store.agent_wakes_since(&channel.id, tern_store::now_ms() - HOUR_MS)?;
            if used >= wakes.wakes_per_hour {
                tracing::info!(post = post.id, agent = %agent.id, "mention not woken: hourly budget");
                self.note(
                    &channel.id,
                    &format!(
                        "@{} not woken: #{}'s hourly wake budget ({}) is used up",
                        agent.slug, channel.slug, wakes.wakes_per_hour
                    ),
                    &format!("nowake:{}:{}", post.id, agent.id),
                )?;
                continue;
            }
            let session = self.agent_thread(&channel, &agent.id)?;
            let woken = self
                .wake_keyed(&agent.id, &session, post.id, &text, taint.as_deref())
                .await?;
            tracing::info!(post = post.id, agent = %agent.id, run = %woken.run_id, queued = woken.queued, "an agent's mention woke an agent");
        }
        Ok(())
    }

    /// [`Manager::wake`] for an agent's post: queued mail is keyed so a
    /// post is never given to one agent twice.
    async fn wake_keyed(
        self: &Arc<Self>,
        agent: &str,
        session: &str,
        post_id: i64,
        text: &str,
        taint: Option<&str>,
    ) -> anyhow::Result<Woken> {
        let store = &self.rt.store;
        let key = format!("wake:{post_id}:{agent}");
        if let Some(run_id) = self.active_run_in(session) {
            store.post_mail(&tern_store::NewMail {
                to_run_id: &run_id,
                from_run_id: None,
                kind: "message",
                body: &serde_json::json!({ "text": text }).to_string(),
                dedupe_key: &key,
            })?;
            if let Some(t) = taint {
                store.set_run_taint(&run_id, t)?;
            }
            return Ok(Woken {
                agent_id: agent.into(),
                run_id,
                session_id: session.into(),
                queued: true,
            });
        }
        self.wake(agent, session, post_id, text, taint, None, "wake")
            .await
    }

    /// Wake agents that agents' posts mention, as the posts are written
    /// (whoever writes them: answers, `channel_post`, routines).
    pub(super) fn watch_mentions(self: &Arc<Self>, shutdown: CancellationToken) {
        let mut rx = self.rt.changes.subscribe();
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                let change = tokio::select! {
                    () = shutdown.cancelled() => return,
                    c = rx.recv() => c,
                };
                match change {
                    Ok(Change::PostCreated { post }) => {
                        if let Err(e) = me.wake_from_post(&post).await {
                            tracing::warn!(post = post.id, error = %e, "waking mentioned agents failed");
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            missed = n,
                            "mention watcher fell behind; some mentions weren't woken"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }
}
