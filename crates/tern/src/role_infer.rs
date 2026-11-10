//! Writing an agent's role in one model call (agent profiles §1.5): from its
//! name alone, and from what it has done, in direct chats and in channels.
//!
//! Used when an agent is created from a name and by the agent sheet's
//! Re-infer. The agent no longer has to wait for a first message to write
//! its role with `role_set`; that tool still refines it over the first
//! turns. The call has no tools, so it can't act on anything it reads. What
//! it reads is data: only the user's words and untainted posts go in, each
//! capped, and the prompt says so.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context;
use futures::StreamExt;
use tern_core::role;
use tern_core::types::{ChatRequest, Message, StreamEvent};
use tern_store::AgentRow;

use crate::runtime::Runtime;

const TIMEOUT: Duration = Duration::from_secs(60);
/// Characters of any one message or post in the evidence.
const ITEM_CHARS: usize = 500;
/// Characters of evidence from direct chats, and from channels.
const DM_CHARS: usize = role::EVIDENCE_CHARS / 2;
const CHANNEL_CHARS: usize = role::EVIDENCE_CHARS / 2;
/// Channels and posts per channel looked at.
const MAX_CHANNELS: usize = 6;
const POSTS_PER_CHANNEL: usize = 30;
const SESSIONS: usize = 12;

#[derive(Debug, PartialEq, Eq)]
pub enum Inferred {
    /// A role was written.
    Written,
    /// Nothing was written, and why (a role exists already, the model
    /// answered with nothing).
    Skipped(String),
}

fn clip(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > ITEM_CHARS {
        let cut: String = text.chars().take(ITEM_CHARS).collect();
        format!("{cut}…")
    } else {
        text
    }
}

/// Lines newest first until `budget` characters, returned oldest first.
fn within(lines: Vec<String>, budget: usize) -> Vec<String> {
    let mut used = 0;
    let mut kept = Vec::new();
    for l in lines.into_iter().rev() {
        used += l.chars().count() + 1;
        if used > budget {
            break;
        }
        kept.push(l);
    }
    kept.reverse();
    kept
}

impl Runtime {
    /// What the agent has been asked and has done, as quoted data: its direct
    /// chats and the channels it took part in. Anything from a run that read
    /// untrusted content is left out (and counted), so outside text never
    /// shapes a role.
    pub fn role_evidence(&self, agent: &AgentRow) -> anyhow::Result<String> {
        let store = &self.store;
        let mut dm = Vec::new();
        let mut channels: Vec<tern_store::ChannelRow> = Vec::new();
        let mut seen = HashSet::new();
        let mut omitted = 0usize;
        // Oldest first, so each list reads in order.
        let mut sessions = store.agent_sessions(&agent.id, SESSIONS)?;
        sessions.reverse();
        for s in &sessions {
            match store.session_channel(&s.id)? {
                Some(c) if c.kind == "topic" => {
                    if seen.insert(c.id.clone()) {
                        channels.push(c);
                    }
                }
                _ => {
                    for m in store.session_messages(&s.id)? {
                        if !matches!(m.role.as_str(), "user" | "assistant")
                            || m.summary_of.is_some()
                            || m.text.trim().is_empty()
                            || m.content_json.contains("\"tool_result\"")
                        {
                            continue;
                        }
                        if let Some(run) = m.run_id.as_deref()
                            && store.run(run)?.is_some_and(|r| r.taint.is_some())
                        {
                            omitted += 1;
                            continue;
                        }
                        let who = if m.role == "user" { "user" } else { "you" };
                        dm.push(format!("- {who}: {}", clip(&m.text)));
                    }
                }
            }
        }
        let mut out = String::new();
        let dm = within(dm, DM_CHARS);
        if !dm.is_empty() {
            out.push_str("Direct chats with the user:\n");
            out.push_str(&dm.join("\n"));
            out.push('\n');
        }
        channels.reverse(); // newest first
        channels.truncate(MAX_CHANNELS);
        let mut lines = Vec::new();
        for c in &channels {
            let mut posts = store.posts(&c.id, None, POSTS_PER_CHANNEL)?;
            posts.reverse();
            let mut mine = false;
            let mut chan = Vec::new();
            for p in posts {
                if p.kind == "note" || p.kind == "pointer" || p.text.trim().is_empty() {
                    continue;
                }
                let me = p.author_id == agent.id;
                mine |= me;
                if p.tainted {
                    omitted += 1;
                    continue;
                }
                let who = if me {
                    "you".to_owned()
                } else if p.author_kind == "user" {
                    "user".to_owned()
                } else {
                    p.author.clone()
                };
                chan.push(format!("- {who}: {}", clip(&p.text)));
            }
            // Only channels it took part in count.
            if mine && !chan.is_empty() {
                lines.push(format!("In channel \"{}\":", clip(&c.title)));
                lines.extend(chan);
            }
        }
        let lines = within(lines, CHANNEL_CHARS);
        if !lines.is_empty() {
            out.push_str("\nChannels it works in (other agents' posts are context only):\n");
            out.push_str(&lines.join("\n"));
            out.push('\n');
        }
        if omitted > 0 && !out.is_empty() {
            out.push_str(&format!(
                "\n({omitted} more left out: they came from runs that read outside content.)\n"
            ));
        }
        Ok(out)
    }

    /// Write `agent`'s role with one model call, from its name and what it
    /// has done. Without `replace` it writes only while the role is open
    /// and empty; with it (Re-infer) the new role replaces the old one, and
    /// the old one stays if the call fails.
    pub async fn infer_role(&self, agent: &AgentRow, replace: bool) -> anyhow::Result<Inferred> {
        match role::load(&self.home, &agent.id) {
            Some(st) if st.open() || replace => {}
            Some(_) => return Ok(Inferred::Skipped("the role is fixed".into())),
            None => anyhow::bail!("this agent doesn't infer its role"),
        }
        if !replace && role::read(&self.home, &agent.id).is_some() {
            return Ok(Inferred::Skipped("it already has a role".into()));
        }
        let evidence = self.role_evidence(agent)?;
        let agent_model = agent
            .model
            .as_deref()
            .and_then(tern_core::config::ModelChain::from_stored)
            .map(|c| c.primary().to_owned());
        let model = self.pick_model_for(None, None, agent_model.as_deref())?;
        let provider = self.agent.provider(&model)?;
        let req = ChatRequest {
            model: model.model.clone(),
            system: role::INFER_SYSTEM.into(),
            messages: vec![Message::user_text(role::infer_request(
                &agent.name,
                &evidence,
            ))],
            tools: Vec::new(),
            max_tokens: 800,
            forbid_tool_calls: true,
        };
        let call = async {
            let mut stream = provider
                .stream(req, tokio_util::sync::CancellationToken::new())
                .await?;
            let mut text = String::new();
            while let Some(ev) = stream.next().await {
                if let StreamEvent::TextDelta(t) = ev? {
                    text.push_str(&t);
                }
            }
            anyhow::Ok(text)
        };
        let reply = tokio::time::timeout(TIMEOUT, call)
            .await
            .map_err(|_| anyhow::anyhow!("the model took over {TIMEOUT:?} to answer"))?
            .context("the model call failed")?;
        let text = role::clean_reply(&reply);
        if text.is_empty() {
            return Ok(Inferred::Skipped("the model wrote no role".into()));
        }
        if replace {
            return match role::replace_inferred(&self.home, &agent.id, &text) {
                Ok(()) => Ok(Inferred::Written),
                Err(e) => Ok(Inferred::Skipped(e)),
            };
        }
        match role::write_inferred_if_empty(&self.home, &agent.id, &text) {
            Ok(true) => Ok(Inferred::Written),
            Ok(false) => Ok(Inferred::Skipped("it already has a role".into())),
            Err(e) => Ok(Inferred::Skipped(e)),
        }
    }
}
