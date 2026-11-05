//! Telegram gateway: long-polls the Bot API, pairs users with one-time
//! codes, streams replies by editing one message (at most once a second),
//! and asks for approvals with inline buttons.

pub mod api;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tern_core::approval::{ApprovalRequest, AuthCard, AuthPrompt, Decision, SecretRequest};
use tern_core::events::AgentEvent;
use tern_core::outbox::OutboxItem;
use tern_store::Store;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{ChatHost, Gateway};
use api::{Api, Buttons, CallbackQuery, MAX_MESSAGE, Message, split_message};

pub const SOURCE: &str = "telegram";

/// What a reply from `author` in channel `slug` starts with: in the
/// channel's own topic, just the author (none for tern); in a chat
/// following it, `#slug · ` first.
fn channel_prefix(slug: &str, in_topic: bool, author: Option<&str>) -> String {
    let who = author
        .filter(|a| *a != tern_store::DEFAULT_AGENT)
        .map(|a| format!("{a} · "))
        .unwrap_or_default();
    if in_topic {
        who
    } else {
        format!("#{slug} · {who}")
    }
}
const EDIT_INTERVAL: Duration = Duration::from_secs(1);
const OFFSET_KEY: &str = "update_offset";

const HELP: &str = "Send me a message and I'll work on it.\n\
/agents — your agents; /agent <name> — talk to one (/agent tern: back)\n\
/channels — your channels; /channel <name> — post in one (/channel home: back)\n\
/new — new topic (I keep a summary of this one)\n\
/model [provider:model] — show or change the model\n\
/stop — stop the current task\n\
/status — what I'm doing\n\
/outbox — actions waiting for your approval";

const AGENT_BOT_HELP: &str = "Send me a message and I'll work on it.\n\
/new — new topic (I keep a summary of this one)\n\
/model [provider:model] — show or change the model\n\
/stop — stop the current task\n\
/status — what I'm doing\n\
/outbox — actions waiting for your approval";

const UNPAIRED: &str = "This bot is private. On the computer running tern, run `tern gateway pair` and send me the code it prints.";

#[derive(Default)]
struct Stream {
    chat: i64,
    /// A forum topic.
    thread: Option<i64>,
    /// `Research · ` when the chat talks to an agent other than the default.
    prefix: String,
    message_id: Option<i64>,
    text: String,
    dirty: bool,
}

/// A message showing an outbox card: (chat, message id, text).
type Card = (i64, i64, String);

pub struct TelegramGateway {
    /// The gateway's name and the `source` of its chats' sessions:
    /// `telegram`, or `telegram-<slug>` for an agent's own bot.
    source: String,
    /// The agent this bot is the chat of (agent profiles §6.4, AP3); the
    /// main bot has none and switches with `/agent`.
    bound_agent: Option<String>,
    api: Api,
    store: Store,
    poll_timeout_secs: u64,
    /// Live replies by run id.
    streams: Mutex<HashMap<String, Stream>>,
    /// Pending approvals by callback id, with the chat they were sent to.
    approvals: Mutex<HashMap<String, (i64, oneshot::Sender<Decision>)>>,
    /// Outbox cards sent, by item: (chat, message id, text), so a decision
    /// made elsewhere updates them like a resolved pointer.
    outbox_cards: Mutex<HashMap<i64, Vec<Card>>>,
}

impl TelegramGateway {
    pub fn new(api_url: &str, token: &str, store: Store) -> Result<Arc<Self>, reqwest::Error> {
        Ok(Arc::new(Self {
            source: SOURCE.to_owned(),
            bound_agent: None,
            api: Api::new(api_url, token)?,
            store,
            poll_timeout_secs: 25,
            streams: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            outbox_cards: Mutex::new(HashMap::new()),
        }))
    }

    /// A bot that is `agent`'s own chat (agent profiles §6.4, AP3): every
    /// chat on it talks to that agent, and nothing switches it. Its
    /// sessions are `source` `telegram-<slug>`; pairing is the main bot's,
    /// per Telegram user, so pairing once trusts you on every bot.
    pub fn for_agent(
        slug: &str,
        agent_id: &str,
        api_url: &str,
        token: &str,
        store: Store,
    ) -> Result<Arc<Self>, reqwest::Error> {
        let source = format!("{SOURCE}-{slug}");
        if let Err(e) = crate::chat_agents::bind_bot(&store, &source, agent_id) {
            tracing::warn!(error = %e, "could not record the bot's agent");
        }
        Ok(Arc::new(Self {
            source,
            bound_agent: Some(agent_id.to_owned()),
            api: Api::new(api_url, token)?,
            store,
            poll_timeout_secs: 25,
            streams: Mutex::new(HashMap::new()),
            approvals: Mutex::new(HashMap::new()),
            outbox_cards: Mutex::new(HashMap::new()),
        }))
    }

    /// Send `item`'s card (buttons unless only the host may decide it) and
    /// remember it.
    async fn send_outbox_card(
        &self,
        chat: i64,
        thread: Option<i64>,
        item: &OutboxItem,
        agent: &str,
    ) {
        let buttons: Buttons = vec![
            ("Approve".into(), format!("ob:{}:approve", item.id)),
            ("Reject".into(), format!("ob:{}:reject", item.id)),
        ];
        let buttons = (!item.local_only).then_some(&buttons);
        let text = outbox_prompt(item, agent);
        match self.api.send_message_in(chat, thread, &text, buttons).await {
            Ok(message) => self
                .outbox_cards
                .lock()
                .await
                .entry(item.id)
                .or_default()
                .push((chat, message, text)),
            Err(e) => tracing::warn!(error = %e, "could not send outbox notice"),
        }
    }

    async fn reply(&self, chat: i64, text: &str) {
        self.reply_in(chat, None, text).await;
    }

    async fn reply_in(&self, chat: i64, thread: Option<i64>, text: &str) {
        for chunk in split_message(text, MAX_MESSAGE) {
            if let Err(e) = self.api.send_message_in(chat, thread, &chunk, None).await {
                tracing::warn!(chat, error = %e, "telegram send failed");
            }
        }
    }

    fn paired(&self, user: i64) -> bool {
        self.store.is_telegram_user_paired(user).unwrap_or(false)
    }

    /// What a reply in `chat_ref` starts with: the agent's name, unless
    /// it's the default agent.
    fn prefix(&self, chat_ref: &str) -> String {
        if self.bound_agent.is_some() {
            return String::new();
        }
        let id = crate::chat_agents::get(&self.store, &self.source, chat_ref);
        match self.store.agent(&id) {
            Ok(Some(a)) if !a.is_default() => format!("{} · ", a.name),
            _ => String::new(),
        }
    }

    /// `/agents`.
    fn agents_text(&self, chat_ref: &str) -> String {
        let current = crate::chat_agents::get(&self.store, &self.source, chat_ref);
        let agents = self.store.agents(false).unwrap_or_default();
        if agents.len() < 2 {
            return "You have one agent, tern. Create more in the tern app or with `tern agents new`."
                .into();
        }
        let lines: Vec<String> = agents
            .iter()
            .map(|a| {
                let here = if a.id == current { "▸" } else { "·" };
                format!("{here} {} (/agent {})", a.name, a.slug)
            })
            .collect();
        format!("Your agents:\n{}", lines.join("\n"))
    }

    /// `/agent <name>`: this chat (or topic) talks to that agent from now
    /// on. In a forum group, a new topic per agent instead, when the bot
    /// may create one.
    async fn switch_agent(&self, msg: &Message, chat_ref: &str, name: &str) {
        let (chat, thread) = (msg.chat.id, msg.message_thread_id);
        if name.is_empty() {
            let text = format!(
                "{}\n{}",
                self.prefix_or_tern(chat_ref),
                self.agents_text(chat_ref)
            );
            return self.reply_in(chat, thread, &text).await;
        }
        let name = name.trim_start_matches('@');
        let agent = self
            .store
            .agents(false)
            .unwrap_or_default()
            .into_iter()
            .find(|a| a.slug == name || a.id == name || a.name.eq_ignore_ascii_case(name));
        let Some(agent) = agent else {
            let text = format!("No agent {name:?}.\n{}", self.agents_text(chat_ref));
            return self.reply_in(chat, thread, &text).await;
        };
        if msg.chat.is_forum && !agent.is_default() {
            match self.api.create_forum_topic(chat, &agent.name).await {
                Ok(topic) => {
                    let topic_ref = format!("{chat}/{topic}");
                    if let Err(e) =
                        crate::chat_agents::set(&self.store, &self.source, &topic_ref, &agent.id)
                    {
                        tracing::warn!(error = %e, "could not save the topic's agent");
                    }
                    self.reply_in(
                        chat,
                        Some(topic),
                        &format!(
                            "{} · This topic is my chat. Write here to talk to me.",
                            agent.name
                        ),
                    )
                    .await;
                    return;
                }
                Err(e) => {
                    tracing::info!(error = %e, "no forum topic for the agent; switching this one");
                }
            }
        }
        if let Err(e) = crate::chat_agents::set(&self.store, &self.source, chat_ref, &agent.id) {
            return self.reply_in(chat, thread, &format!("Error: {e}")).await;
        }
        let text = if agent.is_default() {
            format!("Back to {}.", agent.name)
        } else {
            format!(
                "{} · You're talking to {} now. /agent tern goes back.",
                agent.name, agent.name
            )
        };
        self.reply_in(chat, thread, &text).await;
    }

    /// `/channels`.
    fn channels_text(&self, chat_ref: &str) -> String {
        let current = crate::chat_channels::get(&self.store, &self.source, chat_ref).map(|c| c.id);
        let topics: Vec<_> = self
            .store
            .channels(false)
            .unwrap_or_default()
            .into_iter()
            .filter(|c| c.kind == "topic")
            .collect();
        if topics.is_empty() {
            return "No channels yet. /channel new <name> makes one.".into();
        }
        let lines: Vec<String> = topics
            .iter()
            .map(|c| {
                let here = if current.as_deref() == Some(c.id.as_str()) {
                    "▸"
                } else {
                    "·"
                };
                let waiting = match self.store.open_needs_you(&c.id).unwrap_or(0) {
                    0 => String::new(),
                    n => format!(" ({n} need you)"),
                };
                format!("{here} #{}{waiting} (/channel {})", c.slug, c.slug)
            })
            .collect();
        format!("Your channels:\n{}", lines.join("\n"))
    }

    /// `/channel <name>` (or `new <name> [title]`, or `home`): post in that
    /// channel from now on. Where topics are available (a forum group, or a
    /// private chat with topics, Bot API 9.3–9.4), each channel gets one
    /// topic; otherwise this chat follows it, its messages prefixed
    /// `#slug ·`.
    async fn switch_channel(&self, msg: &Message, chat_ref: &str, arg: &str) {
        let (chat, thread) = (msg.chat.id, msg.message_thread_id);
        let (first, rest) = match arg.split_once(char::is_whitespace) {
            Some((f, r)) => (f, r.trim()),
            None => (arg, ""),
        };
        let slug = match first {
            "" => {
                let text = self.channels_text(chat_ref);
                return self.reply_in(chat, thread, &text).await;
            }
            "home" => {
                let _ = crate::chat_channels::clear(&self.store, &self.source, chat_ref);
                let text = format!("Back to {}.", self.chat_agent_name(chat_ref));
                return self.reply_in(chat, thread, &text).await;
            }
            "new" => {
                let (slug, title) = match rest.split_once(char::is_whitespace) {
                    Some((s, t)) => (s.trim_start_matches('#'), t.trim()),
                    None => (rest.trim_start_matches('#'), ""),
                };
                if let Err(e) = tern_core::channels::check_slug(slug) {
                    return self.reply_in(chat, thread, &format!("Error: {e}")).await;
                }
                if self.store.channel(slug).ok().flatten().is_none()
                    && let Err(e) = self.store.create_channel(&tern_store::ChannelRow {
                        id: tern_core::agent::new_id(),
                        slug: slug.to_owned(),
                        title: if title.is_empty() { slug } else { title }.to_owned(),
                        kind: "topic".into(),
                        created_by: "user".into(),
                        ..Default::default()
                    })
                {
                    return self.reply_in(chat, thread, &format!("Error: {e}")).await;
                }
                slug.to_owned()
            }
            other => other.trim_start_matches('#').to_owned(),
        };
        let channel = self
            .store
            .channel(&slug)
            .ok()
            .flatten()
            .filter(|c| c.kind == "topic" && c.archived_at.is_none());
        let Some(c) = channel else {
            let text = format!("No channel #{slug}.\n{}", self.channels_text(chat_ref));
            return self.reply_in(chat, thread, &text).await;
        };
        // One topic per channel where the chat has topics: the one made
        // before, else a new one if the bot may make it.
        let base = chat.to_string();
        if let Some(topic) = crate::chat_channels::topic(&self.store, &self.source, &base, &c.id)
            .and_then(|t| t.parse::<i64>().ok())
        {
            let text = format!("#{} · This topic is #{}: write there.", c.slug, c.slug);
            return self.reply_in(chat, Some(topic), &text).await;
        }
        match self
            .api
            .create_forum_topic(chat, &format!("#{}", c.slug))
            .await
        {
            Ok(topic) => {
                let topic_ref = format!("{chat}/{topic}");
                let saved = crate::chat_channels::set(&self.store, &self.source, &topic_ref, &c.id)
                    .and_then(|()| {
                        crate::chat_channels::set_topic(
                            &self.store,
                            &self.source,
                            &base,
                            &c.id,
                            &topic.to_string(),
                        )
                    });
                if let Err(e) = saved {
                    tracing::warn!(error = %e, "could not save the channel's topic");
                }
                let text = format!(
                    "#{} · This topic is #{}. Everything said there shows here; write here to post in it.",
                    c.slug, c.slug
                );
                self.reply_in(chat, Some(topic), &text).await;
            }
            Err(e) => {
                // No topics here: this chat follows the channel.
                tracing::info!(error = %e, "no topic for the channel; following it in this chat");
                if let Err(e) =
                    crate::chat_channels::set(&self.store, &self.source, chat_ref, &c.id)
                {
                    return self.reply_in(chat, thread, &format!("Error: {e}")).await;
                }
                let text = format!(
                    "#{} · You're posting in #{} now; its posts show here as #{} · …. /channel home goes back.",
                    c.slug, c.slug, c.slug
                );
                self.reply_in(chat, thread, &text).await;
            }
        }
    }

    /// A post in a channel some chats follow: send it to each, unless it's
    /// the user's own, a quiet note, or the answer a chat asked for (it's
    /// delivered there as the reply).
    async fn on_post(&self, post: &tern_core::channels::Post) {
        if matches!(post.author_kind.as_str(), "user" | "system") || post.kind == "pointer" {
            return;
        }
        // An answer the chat that asked gets delivered.
        let delivered_to = post
            .run_id
            .as_deref()
            .and_then(|r| self.store.run_deliver_to(r).ok().flatten());
        let Some(c) = self.store.channel(&post.channel_id).ok().flatten() else {
            return;
        };
        if c.kind != "topic" {
            return;
        }
        for chat_ref in crate::chat_channels::chats_of(&self.store, &self.source, &c.id) {
            if delivered_to.as_deref() == Some(format!("{}:{chat_ref}", self.source).as_str()) {
                continue;
            }
            let Some((chat, thread)) = place(&chat_ref) else {
                continue;
            };
            let prefix = channel_prefix(&c.slug, thread.is_some(), Some(&post.author));
            let waiting = if post.kind == "needs_you" && post.resolved_at.is_none() {
                " (needs you)"
            } else {
                ""
            };
            let text = format!("{prefix}{}{waiting}", post.text.trim());
            self.reply_in(chat, thread, &text).await;
        }
    }

    /// An agent's current name (the default agent can be renamed).
    fn agent_name(&self, id: &str) -> String {
        self.store
            .agent(id)
            .ok()
            .flatten()
            .map_or_else(|| "tern".to_owned(), |a| a.name)
    }

    /// The name of the agent `chat_ref` talks to.
    fn chat_agent_name(&self, chat_ref: &str) -> String {
        self.agent_name(&crate::chat_agents::get(
            &self.store,
            &self.source,
            chat_ref,
        ))
    }

    fn prefix_or_tern(&self, chat_ref: &str) -> String {
        let id = crate::chat_agents::get(&self.store, &self.source, chat_ref);
        let name = self
            .store
            .agent(&id)
            .ok()
            .flatten()
            .map_or_else(|| "tern".to_owned(), |a| a.name);
        format!("This chat talks to {name}.")
    }

    async fn on_message(&self, host: &Arc<dyn ChatHost>, msg: Message) {
        let chat = msg.chat.id;
        // In a forum group, or a private chat with topics (Bot API 9.3),
        // each topic is its own chat (agent profiles §6.4).
        let thread = msg
            .message_thread_id
            .filter(|_| msg.chat.is_forum || msg.is_topic_message);
        let Some(text) = msg.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
            return;
        };
        let Some(user) = msg.from.as_ref().map(|u| u.id) else {
            return;
        };
        if !self.paired(user) {
            let code = text.strip_prefix("/start").map(str::trim).unwrap_or(text);
            if !code.is_empty() && self.store.consume_pairing_code(code).unwrap_or(false) {
                let _ = self.store.pair_telegram_user(user);
                tracing::info!(user, "telegram user paired");
                let help = if self.bound_agent.is_some() {
                    AGENT_BOT_HELP
                } else {
                    HELP
                };
                self.reply(chat, &format!("Paired. {help}")).await;
            } else {
                self.reply(chat, UNPAIRED).await;
            }
            return;
        }

        let chat_ref = match thread {
            Some(t) => format!("{chat}/{t}"),
            None => chat.to_string(),
        };
        let (cmd, arg) = match text.split_once(char::is_whitespace) {
            Some((c, a)) => (c, a.trim()),
            None => (text, ""),
        };
        // Commands may arrive as `/stop@my_bot` in groups.
        let cmd = cmd.split('@').next().unwrap_or(cmd);
        if let Some(agent) = &self.bound_agent
            && matches!(cmd, "/agents" | "/agent" | "/channels" | "/channel")
        {
            let name = self.agent_name(agent);
            let text = format!(
                "This bot is {name}'s own chat, so there is nothing to switch. Use your main tern bot for other agents and channels."
            );
            return self.reply_in(chat, thread, &text).await;
        }
        match cmd {
            "/start" | "/help" if self.bound_agent.is_some() => {
                self.reply_in(chat, thread, AGENT_BOT_HELP).await
            }
            "/start" | "/help" => self.reply_in(chat, thread, HELP).await,
            "/agents" => {
                let text = self.agents_text(&chat_ref);
                self.reply_in(chat, thread, &text).await;
            }
            "/agent" => {
                if let Some(c) = crate::chat_channels::get(&self.store, &self.source, &chat_ref) {
                    if thread.is_some() {
                        let text = format!(
                            "#{} · This topic is #{}. Use /agent in the main chat.",
                            c.slug, c.slug
                        );
                        return self.reply_in(chat, thread, &text).await;
                    }
                    // A chat follows one channel or agent at a time.
                    let _ = crate::chat_channels::clear(&self.store, &self.source, &chat_ref);
                }
                self.switch_agent(&msg, &chat_ref, arg).await
            }
            "/channels" => {
                let text = self.channels_text(&chat_ref);
                self.reply_in(chat, thread, &text).await;
            }
            "/channel" => self.switch_channel(&msg, &chat_ref, arg).await,
            "/new" if crate::chat_channels::get(&self.store, &self.source, &chat_ref).is_some() => {
                self.reply_in(
                    chat,
                    thread,
                    "A channel is one conversation, without topics of its own. /channel home goes back.",
                )
                .await
            }
            "/new" => match host.new_session(&self.source, &chat_ref).await {
                Ok(()) => {
                    let text = format!(
                        "{}New topic. I kept a summary of the last one.",
                        self.prefix(&chat_ref)
                    );
                    self.reply_in(chat, thread, &text).await
                }
                Err(e) => self.reply_in(chat, thread, &format!("Error: {e}")).await,
            },
            "/model" => {
                let set = (!arg.is_empty()).then_some(arg);
                match host.model(&self.source, &chat_ref, set).await {
                    Ok(m) if set.is_some() => {
                        self.reply_in(chat, thread, &format!("Model set to {m}."))
                            .await
                    }
                    Ok(m) => self.reply_in(chat, thread, &format!("Model: {m}")).await,
                    Err(e) => self.reply_in(chat, thread, &format!("Error: {e}")).await,
                }
            }
            "/stop" => {
                let msg = if host.stop(&self.source, &chat_ref).await {
                    "Stopping."
                } else {
                    "Nothing is running."
                };
                self.reply_in(chat, thread, msg).await;
            }
            "/outbox" => {
                let items = host.pending_outbox().await;
                if items.is_empty() {
                    self.reply_in(chat, thread, "Nothing is waiting for you.")
                        .await;
                }
                for item in items.iter().take(10) {
                    let agent = match item.agent_id.as_deref() {
                        Some(id) => self.agent_name(id),
                        None => self.chat_agent_name(&chat_ref),
                    };
                    self.send_outbox_card(chat, thread, item, &agent).await;
                }
                if items.len() > 10 {
                    let more = format!("{} more: `tern outbox` lists them all.", items.len() - 10);
                    self.reply_in(chat, thread, &more).await;
                }
            }
            "/status" => {
                let text = format!(
                    "{}{}",
                    self.prefix(&chat_ref),
                    host.status(&self.source, &chat_ref).await
                );
                self.reply_in(chat, thread, &text).await
            }
            _ if let Some(c) = crate::chat_channels::get(&self.store, &self.source, &chat_ref) => {
                self.api.typing(chat).await;
                match host.send_to_channel(&self.source, &chat_ref, &c.id, text).await {
                    Ok((run_id, agent)) => {
                        let prefix = channel_prefix(&c.slug, thread.is_some(), Some(&agent));
                        self.streams
                            .lock()
                            .await
                            .entry(run_id)
                            .or_insert_with(|| Stream {
                                chat,
                                thread,
                                prefix,
                                ..Stream::default()
                            });
                    }
                    Err(e) => self.reply_in(chat, thread, &format!("Error: {e}")).await,
                }
            }
            _ => {
                self.api.typing(chat).await;
                match host.send(&self.source, &chat_ref, text).await {
                    // A queued message returns the run already streaming here.
                    Ok(run_id) => {
                        let prefix = self.prefix(&chat_ref);
                        self.streams
                            .lock()
                            .await
                            .entry(run_id)
                            .or_insert_with(|| Stream {
                                chat,
                                thread,
                                prefix,
                                ..Stream::default()
                            });
                    }
                    Err(e) => self.reply_in(chat, thread, &format!("Error: {e}")).await,
                }
            }
        }
    }

    async fn on_callback(&self, host: &Arc<dyn ChatHost>, cb: CallbackQuery) {
        if !self.paired(cb.from.id) {
            let _ = self.api.answer_callback(&cb.id, "Not paired").await;
            return;
        }
        let data = cb.data.clone().unwrap_or_default();
        let mut parts = data.splitn(3, ':');
        let (kind, id, choice) = (parts.next(), parts.next(), parts.next());
        if let (Some("ob"), Some(id), Some(choice)) = (kind, id, choice) {
            return self.on_outbox_callback(host, cb, id, choice).await;
        }
        let (Some("ap"), Some(id), Some(choice)) = (kind, id, choice) else {
            let _ = self.api.answer_callback(&cb.id, "").await;
            return;
        };
        let decision = match choice {
            "once" => Decision::AllowOnce,
            "always" => Decision::AlwaysAllow,
            "run" => Decision::AllowRun,
            "host" => Decision::AlwaysAllowHost,
            _ => Decision::Deny,
        };
        let label = match decision {
            Decision::AllowOnce => "Allowed",
            Decision::AlwaysAllow => "Always allowed",
            Decision::AllowRun => "Allowed all sites for this run",
            Decision::AllowSession => "Allowed for this conversation",
            Decision::AlwaysAllowHost => "Site always allowed",
            Decision::Deny | Decision::Unanswered => "Denied",
        };
        // Only a tap in the chat the request was sent to counts.
        let chat = cb.message.as_ref().map(|m| m.chat.id);
        let pending = {
            let mut approvals = self.approvals.lock().await;
            match approvals.get(id) {
                Some((target, _)) if Some(*target) == chat => approvals.remove(id),
                _ => None,
            }
        };
        let answered = pending.is_some_and(|(_, tx)| tx.send(decision).is_ok());
        let _ = self
            .api
            .answer_callback(&cb.id, if answered { label } else { "Expired" })
            .await;
        if let Some(m) = cb.message {
            let original = m.text.unwrap_or_default();
            let status = if answered { label } else { "Expired" };
            let _ = self
                .api
                .edit_message(
                    m.chat.id,
                    m.message_id,
                    &format!("{original}\n→ {status}"),
                    None,
                )
                .await;
        }
    }

    /// Fold agent events into live replies.
    async fn on_event(&self, ev: AgentEvent) {
        let mut streams = self.streams.lock().await;
        match ev {
            AgentEvent::TextDelta { run_id, text } => {
                if let Some(s) = streams.get_mut(&run_id) {
                    s.text.push_str(&text);
                    s.dirty = true;
                }
            }
            AgentEvent::ToolStarted {
                run_id, summary, ..
            } => {
                if let Some(s) = streams.get_mut(&run_id) {
                    if !s.text.is_empty() && !s.text.ends_with('\n') {
                        s.text.push('\n');
                    }
                    s.text.push_str(&format!("▸ {summary}\n"));
                    s.dirty = true;
                }
            }
            AgentEvent::RunFinished {
                run_id,
                status,
                error,
                ..
            } if status != "completed" => {
                // Completed runs are finalized by `deliver`.
                if let Some(s) = streams.remove(&run_id) {
                    let note = match error {
                        Some(e) => format!("Run failed: {e}"),
                        None => "(stopped)".to_owned(),
                    };
                    let text = if s.text.trim().is_empty() {
                        note
                    } else {
                        format!("{}\n\n{note}", s.text.trim_end())
                    };
                    drop(streams);
                    self.put(
                        s.chat,
                        s.thread,
                        s.message_id,
                        &format!("{}{text}", s.prefix),
                    )
                    .await;
                }
            }
            _ => {}
        }
    }

    /// Show `text` in `message_id` (or a new message), splitting if needed.
    async fn put(&self, chat: i64, thread: Option<i64>, message_id: Option<i64>, text: &str) {
        let chunks = split_message(
            if text.trim().is_empty() {
                "(no response)"
            } else {
                text
            },
            MAX_MESSAGE,
        );
        let mut rest = chunks.iter();
        if let Some(id) = message_id
            && let Some(first) = rest.next()
            && let Err(e) = self.api.edit_message(chat, id, first, None).await
        {
            tracing::warn!(chat, error = %e, "telegram edit failed");
        }
        for chunk in rest {
            if let Err(e) = self.api.send_message_in(chat, thread, chunk, None).await {
                tracing::warn!(chat, error = %e, "telegram send failed");
            }
        }
    }

    /// Push pending streamed text, at most once per `EDIT_INTERVAL`.
    async fn flush(&self) {
        let mut streams = self.streams.lock().await;
        for s in streams.values_mut().filter(|s| s.dirty) {
            s.dirty = false;
            let shown = live_view(&format!("{}{}", s.prefix, s.text));
            match s.message_id {
                None => match self
                    .api
                    .send_message_in(s.chat, s.thread, &shown, None)
                    .await
                {
                    Ok(id) => s.message_id = Some(id),
                    Err(e) => tracing::warn!(error = %e, "telegram send failed"),
                },
                Some(id) => {
                    if let Err(e) = self.api.edit_message(s.chat, id, &shown, None).await {
                        tracing::warn!(error = %e, "telegram edit failed");
                    }
                }
            }
        }
    }
}

/// A chat ref: `<chat>` or, for a forum topic, `<chat>/<thread>`.
fn place(target: &str) -> Option<(i64, Option<i64>)> {
    match target.split_once('/') {
        Some((c, t)) => Some((c.parse().ok()?, Some(t.parse().ok()?))),
        None => Some((target.parse().ok()?, None)),
    }
}

/// What a still-streaming reply shows: the tail, if it's over the limit.
/// "Allow destructive · shell git push?", prefixed with a sub-agent's label when one asks,
/// and followed by why a tainted run asks.
pub(crate) fn approval_prompt(req: &ApprovalRequest) -> String {
    let ask = match &req.agent_label {
        Some(l) => format!("[{l}] Allow {}?", req.headline()),
        None => format!("Allow {}?", req.headline()),
    };
    match req.taint_note() {
        Some(note) => format!("{ask}\n⚠ {note}"),
        None => ask,
    }
}

impl TelegramGateway {
    async fn on_outbox_callback(
        &self,
        host: &Arc<dyn ChatHost>,
        cb: CallbackQuery,
        id: &str,
        choice: &str,
    ) {
        let outcome = match id.parse::<i64>() {
            Ok(id) => {
                // This card is updated below; the others when it resolves.
                if let Some(m) = &cb.message
                    && let Some(cards) = self.outbox_cards.lock().await.get_mut(&id)
                {
                    cards.retain(|(c, msg, _)| (*c, *msg) != (m.chat.id, m.message_id));
                }
                host.decide_outbox(id, choice == "approve").await
            }
            Err(_) => Err("unknown item".into()),
        };
        let status = match &outcome {
            Ok(s) => s.clone(),
            Err(e) => e.clone(),
        };
        let _ = self.api.answer_callback(&cb.id, &status).await;
        if let Some(m) = cb.message {
            let original = m.text.unwrap_or_default();
            let _ = self
                .api
                .edit_message(
                    m.chat.id,
                    m.message_id,
                    &format!("{original}\n→ {status}"),
                    None,
                )
                .await;
        }
    }
}

/// "1 action waiting" for the user, with what it is and why it waits.
pub(crate) fn outbox_prompt(item: &OutboxItem, agent: &str) -> String {
    let mut text = format!(
        "📬 Waiting for you (outbox #{}): {}\nNobody was here to approve it, so {agent} carried on without it.",
        item.id,
        item.headline()
    );
    if let Some(taint) = &item.taint {
        text.push_str(&format!("\n⚠ {}", tern_core::approval::taint_note(taint)));
    }
    if item.local_only {
        text.push_str("\nApprove it in the tern app or TUI on the computer running tern.");
    }
    text
}

fn live_view(text: &str) -> String {
    let n = text.chars().count();
    if n <= MAX_MESSAGE - 2 {
        return format!("{text} ▌");
    }
    let tail: String = text.chars().skip(n - (MAX_MESSAGE - 4)).collect();
    format!("…{tail} ▌")
}

#[async_trait]
impl Gateway for TelegramGateway {
    fn name(&self) -> &str {
        self.source.as_str()
    }

    fn kind(&self) -> &str {
        SOURCE
    }

    async fn run(self: Arc<Self>, host: Arc<dyn ChatHost>, cancel: CancellationToken) {
        let events = {
            let (me, mut rx, cancel) = (self.clone(), host.events(), cancel.clone());
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        ev = rx.recv() => match ev {
                            Ok(ev) => me.on_event(ev).await,
                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => return,
                        },
                        () = cancel.cancelled() => return,
                    }
                }
            })
        };
        // Channels' posts reach the chats following them (C5).
        let posts = host.changes().map(|mut rx| {
            let (me, cancel) = (self.clone(), cancel.clone());
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        c = rx.recv() => match c {
                            Ok(tern_core::models::Change::PostCreated { post }) => me.on_post(&post).await,
                            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => return,
                        },
                        () = cancel.cancelled() => return,
                    }
                }
            })
        });
        let flusher = {
            let (me, cancel) = (self.clone(), cancel.clone());
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(EDIT_INTERVAL);
                loop {
                    tokio::select! {
                        _ = tick.tick() => me.flush().await,
                        () = cancel.cancelled() => return,
                    }
                }
            })
        };

        // Persisted so a restart never handles the same message twice.
        let mut offset: i64 = self
            .store
            .kv_get(&self.source, OFFSET_KEY)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut backoff = Duration::from_secs(1);
        loop {
            let updates = tokio::select! {
                u = self.api.get_updates(offset, self.poll_timeout_secs) => u,
                () = cancel.cancelled() => break,
            };
            match updates {
                Ok(updates) => {
                    backoff = Duration::from_secs(1);
                    if let Some(last) = updates.iter().map(|u| u.update_id).max() {
                        offset = offset.max(last + 1);
                        if let Err(e) =
                            self.store
                                .kv_set(&self.source, OFFSET_KEY, &offset.to_string())
                        {
                            tracing::warn!(error = %e, "could not save telegram offset");
                        }
                    }
                    for u in updates {
                        if let Some(m) = u.message {
                            // Handle each message on its own task so a slow
                            // command can't block approvals arriving meanwhile.
                            let (me, host) = (self.clone(), host.clone());
                            tokio::spawn(async move { me.on_message(&host, m).await });
                        } else if let Some(cb) = u.callback_query {
                            self.on_callback(&host, cb).await;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, retry_in = ?backoff, "telegram polling failed");
                    tokio::select! {
                        () = tokio::time::sleep(backoff) => {}
                        () = cancel.cancelled() => break,
                    }
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
        events.abort();
        flusher.abort();
        if let Some(p) = posts {
            p.abort();
        }
    }

    async fn deliver(&self, target: &str, run_id: Option<&str>, text: &str) -> Result<(), String> {
        let (chat, thread) =
            place(target).ok_or_else(|| format!("bad telegram chat id {target:?}"))?;
        let stream = match run_id {
            Some(r) => self.streams.lock().await.remove(r),
            None => None,
        };
        let prefix = match &stream {
            Some(s) => s.prefix.clone(),
            None => self.prefix(target),
        };
        self.put(
            chat,
            thread,
            stream.and_then(|s| s.message_id),
            &format!("{prefix}{text}"),
        )
        .await;
        Ok(())
    }

    async fn ask_approval(&self, target: &str, req: &ApprovalRequest) -> Decision {
        let Some((chat, thread)) = place(target) else {
            return Decision::Deny;
        };
        // Callback data is limited to 64 bytes.
        let id: String = req
            .id
            .chars()
            .rev()
            .take(16)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let (tx, rx) = oneshot::channel();
        self.approvals.lock().await.insert(id.clone(), (chat, tx));
        let mut buttons: Buttons = vec![("Allow once".into(), format!("ap:{id}:once"))];
        if let Some(pattern) = &req.pattern {
            buttons.push((format!("Always ({pattern})"), format!("ap:{id}:always")));
        }
        // The tainted host limit's choices.
        if let Some(host) = &req.egress_host {
            buttons.push(("All sites this run".into(), format!("ap:{id}:run")));
            buttons.push((format!("Always {host}"), format!("ap:{id}:host")));
        }
        buttons.push(("Deny".into(), format!("ap:{id}:deny")));
        if let Err(e) = self
            .api
            .send_message_in(chat, thread, &approval_prompt(req), Some(&buttons))
            .await
        {
            tracing::warn!(error = %e, "could not send approval request");
            self.approvals.lock().await.remove(&id);
            return Decision::Unanswered;
        }
        // If the agent stops waiting (timeout, cancel), this future is
        // dropped; the stale entry then answers "Expired".
        rx.await.unwrap_or(Decision::Deny)
    }

    async fn outbox_notice(&self, target: &str, item: &OutboxItem) {
        let Some((chat, thread)) = place(target) else {
            return;
        };
        let agent = match item.agent_id.as_deref() {
            Some(id) => self.agent_name(id),
            None => self.chat_agent_name(target),
        };
        self.send_outbox_card(chat, thread, item, &agent).await;
    }

    async fn outbox_resolved(&self, item: &OutboxItem) {
        let Some(cards) = self.outbox_cards.lock().await.remove(&item.id) else {
            return;
        };
        let status = match item.state.as_str() {
            "rejected" => "Rejected",
            "expired" => "Expired",
            "failed" => "Approved; it failed",
            _ => "Approved",
        };
        for (chat, message, text) in cards {
            let _ = self
                .api
                .edit_message(chat, message, &format!("{text}\n→ {status}"), None)
                .await;
        }
    }

    async fn secret_request_notice(&self, target: &str, req: &SecretRequest) -> Option<String> {
        let (chat, thread) = place(target)?;
        match self
            .api
            .send_message_in(
                chat,
                thread,
                &secret_notice(req, &self.chat_agent_name(target)),
                None,
            )
            .await
        {
            Ok(id) => Some(id.to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "could not send secret request notice");
                None
            }
        }
    }

    async fn auth_prompt_notice(&self, target: &str, req: &AuthPrompt) -> Option<String> {
        let (chat, thread) = place(target)?;
        match self
            .api
            .send_message_in(
                chat,
                thread,
                &auth_notice(req, &self.chat_agent_name(target)),
                None,
            )
            .await
        {
            Ok(id) => Some(id.to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "could not send sign-in notice");
                None
            }
        }
    }

    async fn secret_request_done(&self, target: &str, handle: &str, text: &str) {
        let (Some((chat, _)), Ok(id)) = (place(target), handle.parse::<i64>()) else {
            return;
        };
        if let Err(e) = self.api.edit_message(chat, id, text, None).await {
            tracing::warn!(error = %e, "could not update secret request notice");
        }
    }
}

/// Points the user at an app that can take the value: never this chat.
pub(crate) fn secret_notice(req: &SecretRequest, agent: &str) -> String {
    let names: Vec<&str> = req.asks.iter().map(|a| a.name.as_str()).collect();
    let who = match &req.agent_label {
        Some(l) => format!("Sub-agent {l}"),
        None => agent.to_owned(),
    };
    let mut text = format!(
        "🔑 {who} needs {} to continue.\nThe agent says: \"{}\"\n\n",
        names.join(", "),
        req.reason
    );
    text.push_str(
        "Enter it in the tern app or the TUI, not here: chat messages keep it in the history. \
         It's waiting in the conversation, or set it with /secrets in the TUI.",
    );
    text
}

/// Points the user at the app or TUI, where the card is: a sign-in opens a
/// browser on tern's machine, and a choice belongs with it.
fn auth_notice(req: &AuthPrompt, agent: &str) -> String {
    let who = match &req.agent_label {
        Some(l) => format!("Sub-agent {l}"),
        None => agent.to_owned(),
    };
    match &req.card {
        AuthCard::Choose { title, .. } => format!(
            "🔐 {who} wants you to choose how to connect: {title}.\nIt's waiting in the tern app or the TUI."
        ),
        AuthCard::SignIn { title, .. } => format!(
            "🔐 {who} needs you to sign in: {title}.\nFinish it in the tern app or the TUI; the sign-in page opens there, not in this chat."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_view_shows_the_tail_within_limits() {
        assert_eq!(live_view("hi"), "hi ▌");
        let long = "a".repeat(5000) + "END";
        let v = live_view(&long);
        assert!(v.chars().count() <= MAX_MESSAGE);
        assert!(v.starts_with('…') && v.contains("END"));
    }
}
