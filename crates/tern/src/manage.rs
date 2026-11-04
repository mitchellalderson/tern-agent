//! Editing and deleting what users create: the rules shared by the API,
//! `tern agents` / `tern channels` and the TUI (DECISIONS.md "Editing and
//! deleting"). Deleting an agent hands its routines on or deletes them as
//! asked, refuses while it works, and leaves its history readable; deleting
//! a channel keeps its conversations. Nothing here deletes without the
//! caller having decided: the surfaces ask first.

use std::fmt;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tern_store::{AgentRow, ChannelRow, DEFAULT_AGENT, RoutineFate};

use crate::runtime::Runtime;

/// Why a change was refused (the caller's problem, shown as is) or failed.
#[derive(Debug)]
pub enum ManageError {
    Refused(String),
    Other(anyhow::Error),
}

impl fmt::Display for ManageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(m) => f.write_str(m),
            Self::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for ManageError {}

impl From<anyhow::Error> for ManageError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

impl From<tern_store::StoreError> for ManageError {
    fn from(e: tern_store::StoreError) -> Self {
        match e {
            tern_store::StoreError::Invalid(m) => Self::Refused(m),
            e => Self::Other(e.into()),
        }
    }
}

fn refuse<T>(msg: impl Into<String>) -> Result<T, ManageError> {
    Err(ManageError::Refused(msg.into()))
}

/// A routine or channel named in an impact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Named {
    pub id: String,
    /// A routine's prompt (first line); a channel's slug.
    pub label: String,
}

fn named(v: Vec<(String, String)>) -> Vec<Named> {
    v.into_iter()
        .map(|(id, label)| Named { id, label })
        .collect()
}

/// What deleting an agent touches, and whether it can be deleted now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentImpact {
    pub id: String,
    pub slug: String,
    pub name: String,
    /// Its routines: handed to another agent or deleted with it.
    pub routines: Vec<Named>,
    /// Topic channels it leads: they go to the agent taking its routines,
    /// else to `tern`.
    pub led_channels: Vec<Named>,
    /// Topic channels it is in: it leaves them.
    pub member_channels: Vec<Named>,
    /// Its conversations: they stay in History under its name.
    pub conversations: usize,
    /// Its private memories: deleted with it.
    pub memories: usize,
    pub running_runs: usize,
    pub pending_outbox: usize,
    /// Reasons it can't be deleted at all right now.
    pub blockers: Vec<String>,
}

impl AgentImpact {
    /// The impact as words, for a confirmation.
    pub fn describe(&self) -> String {
        let mut lines = vec![format!("Delete @{} ({})?", self.slug, self.name)];
        lines.push(format!(
            "- its {} conversation(s) stay in History under its name",
            self.conversations
        ));
        if self.memories > 0 {
            lines.push(format!(
                "- its {} private memor{} will be deleted",
                self.memories,
                if self.memories == 1 { "y" } else { "ies" }
            ));
        }
        if !self.routines.is_empty() {
            lines.push(format!(
                "- it owns {} routine(s): {}; hand them to another agent or delete them",
                self.routines.len(),
                self.routines
                    .iter()
                    .map(|r| r.label.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        if !self.led_channels.is_empty() {
            lines.push(format!(
                "- it leads {}; the lead moves to the agent taking its routines, else to tern",
                tags(&self.led_channels)
            ));
        }
        if !self.member_channels.is_empty() {
            lines.push(format!("- it leaves {}", tags(&self.member_channels)));
        }
        lines.push("its persona file is kept under ~/.tern/agents/.deleted/".into());
        for b in &self.blockers {
            lines.push(format!("cannot delete now: {b}"));
        }
        lines.join("\n")
    }
}

fn tags(v: &[Named]) -> String {
    v.iter()
        .map(|c| format!("#{}", c.label))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What to do with an agent's routines when deleting it.
#[derive(Debug, Clone, Default)]
pub struct DeleteAgent {
    /// Hand its routines (and led channels) to this agent.
    pub reassign_to: Option<String>,
    /// Delete its routines instead.
    pub delete_routines: bool,
}

/// What deleting a channel touches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelImpact {
    pub id: String,
    pub slug: String,
    pub title: String,
    /// Posts deleted with it.
    pub posts: usize,
    /// Conversations that move under Home, so History keeps them.
    pub conversations: usize,
    /// Routines that posted here: they post to their agent's DM instead.
    pub routines: Vec<Named>,
    pub members: usize,
    pub running_runs: usize,
    pub blockers: Vec<String>,
}

impl ChannelImpact {
    pub fn describe(&self) -> String {
        let mut lines = vec![format!("Delete #{} ({})?", self.slug, self.title)];
        lines.push(format!("- its {} post(s) will be deleted", self.posts));
        lines.push(format!(
            "- its {} conversation(s) stay in History",
            self.conversations
        ));
        if !self.routines.is_empty() {
            lines.push(format!(
                "- {} routine(s) post here ({}); they post to their agent's DM instead",
                self.routines.len(),
                self.routines
                    .iter()
                    .map(|r| r.label.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        for b in &self.blockers {
            lines.push(format!("cannot delete now: {b}"));
        }
        lines.join("\n")
    }
}

impl Runtime {
    /// Rename agent `a`: its display name, and its @name when `slug` is
    /// given (its DM follows; its id and history don't change). The
    /// default agent keeps the @name `tern`.
    pub fn rename_agent(
        &self,
        a: &AgentRow,
        name: Option<&str>,
        slug: Option<&str>,
    ) -> Result<AgentRow, ManageError> {
        let name = name.map(str::trim);
        if name.is_none() && slug.is_none() {
            return refuse("give a new name, or a new @name");
        }
        if name == Some("") {
            return refuse("name is empty");
        }
        let slug = slug.map(|s| s.trim().trim_start_matches('@'));
        if let Some(s) = slug {
            if a.is_default() && s != DEFAULT_AGENT {
                return refuse(
                    "the default agent's @name stays `tern`; rename its display name instead",
                );
            }
            tern_core::agents::check_slug(s).map_err(ManageError::Refused)?;
            if self
                .store
                .agent(s)
                .map_err(anyhow::Error::from)?
                .is_some_and(|o| o.id != a.id)
            {
                return refuse(format!("there is already an agent @{s}"));
            }
            if self
                .store
                .channel(&tern_store::dm_slug(s))
                .map_err(anyhow::Error::from)?
                .is_some_and(|c| c.agent_id.as_deref() != Some(a.id.as_str()))
            {
                return refuse(format!("the channel #{} is taken", tern_store::dm_slug(s)));
            }
        }
        self.store
            .update_agent(
                &a.id,
                &tern_store::AgentChange {
                    name: name.map(str::to_owned),
                    slug: slug.map(str::to_owned),
                    ..tern_store::AgentChange::default()
                },
            )
            .map_err(anyhow::Error::from)?;
        self.store
            .agent(&a.id)
            .map_err(anyhow::Error::from)?
            .ok_or_else(|| ManageError::Refused("no such agent".into()))
    }

    /// What deleting `a` would touch.
    pub fn agent_impact(&self, a: &AgentRow) -> Result<AgentImpact, ManageError> {
        let u = self.store.agent_use(&a.id).map_err(anyhow::Error::from)?;
        let mut blockers = Vec::new();
        if a.is_default() {
            blockers.push("the default agent can't be deleted; rename it instead".to_owned());
        }
        if u.running_runs > 0 {
            blockers.push(format!(
                "{} run(s) of @{} are in progress: wait for them or stop them first",
                u.running_runs, a.slug
            ));
        }
        if u.pending_outbox > 0 {
            blockers.push(format!(
                "{} of its writes are waiting in the outbox: approve or reject them first",
                u.pending_outbox
            ));
        }
        Ok(AgentImpact {
            id: a.id.clone(),
            slug: a.slug.clone(),
            name: a.name.clone(),
            routines: named(u.routines),
            led_channels: named(u.led_channels),
            member_channels: named(u.member_channels),
            conversations: u.conversations,
            memories: u.memories,
            running_runs: u.running_runs,
            pending_outbox: u.pending_outbox,
            blockers,
        })
    }

    /// Delete agent `a` (the caller has confirmed). Refuses the default
    /// agent, an agent with runs in progress or writes in the outbox, and
    /// one with routines unless `opts` says what becomes of them.
    pub fn delete_agent(
        &self,
        a: &AgentRow,
        opts: &DeleteAgent,
    ) -> Result<AgentImpact, ManageError> {
        let impact = self.agent_impact(a)?;
        if let Some(b) = impact.blockers.first() {
            return refuse(b.clone());
        }
        if opts.reassign_to.is_some() && opts.delete_routines {
            return refuse("choose one: hand its routines to another agent or delete them");
        }
        if !impact.routines.is_empty() && opts.reassign_to.is_none() && !opts.delete_routines {
            return refuse(format!(
                "@{} owns {} routine(s) ({}): hand them to another agent (--reassign-to) or \
                 delete them (--delete-routines)",
                a.slug,
                impact.routines.len(),
                impact
                    .routines
                    .iter()
                    .map(|r| r.label.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        let target = match opts.reassign_to.as_deref() {
            Some(t) => {
                let t = self
                    .store
                    .agent(t.trim_start_matches('@'))
                    .map_err(anyhow::Error::from)?
                    .filter(|t| t.archived_at.is_none());
                match t {
                    Some(t) if t.id != a.id => Some(t),
                    Some(_) => return refuse("an agent can't hand its routines to itself"),
                    None => return refuse("the agent to hand its routines to doesn't exist"),
                }
            }
            None => None,
        };
        let new_lead = target.as_ref().map_or(DEFAULT_AGENT, |t| t.id.as_str());
        for c in &impact.led_channels {
            self.store
                .set_channel_lead(&c.id, new_lead)
                .map_err(anyhow::Error::from)?;
        }
        let fate = match &target {
            Some(t) => RoutineFate::Move(t.id.clone()),
            None => RoutineFate::Delete,
        };
        if !self.store.delete_agent(&a.id, &fate)? {
            return refuse("no such agent");
        }
        self.keep_agent_files(a);
        self.refresh_agent_chains();
        Ok(impact)
    }

    /// Move a deleted agent's persona and private memory out of the way,
    /// to `agents/.deleted/<name>-<time>`, so deleting loses nothing a
    /// user could want back. Best effort: the agent is already gone.
    fn keep_agent_files(&self, a: &AgentRow) {
        let dir = tern_core::agents::dir(&self.home, &a.id);
        if !dir.is_dir() {
            return;
        }
        let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
            return;
        };
        let graveyard = dir.with_file_name(".deleted");
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let dest = graveyard.join(format!("{name}-{stamp}"));
        if std::fs::create_dir_all(&graveyard).is_ok()
            && let Err(e) = std::fs::rename(&dir, &dest)
        {
            tracing::warn!("keeping {} of deleted agent {}: {e}", dir.display(), a.id);
        }
    }

    /// What deleting topic channel `c` would touch.
    pub fn channel_impact(&self, c: &ChannelRow) -> Result<ChannelImpact, ManageError> {
        let u = self.store.channel_use(&c.id).map_err(anyhow::Error::from)?;
        let mut blockers = Vec::new();
        if c.kind == "main" {
            blockers.push(if c.id == tern_store::HOME {
                "Home can't be deleted".to_owned()
            } else {
                "an agent's DM goes with its agent: delete the agent".to_owned()
            });
        }
        if u.running_runs > 0 {
            blockers.push(format!(
                "{} run(s) are in progress in #{}: wait for them or stop them first",
                u.running_runs, c.slug
            ));
        }
        Ok(ChannelImpact {
            id: c.id.clone(),
            slug: c.slug.clone(),
            title: c.title.clone(),
            posts: u.posts,
            conversations: u.conversations,
            routines: named(u.routines),
            members: u.members,
            running_runs: u.running_runs,
            blockers,
        })
    }

    /// Delete topic channel `c` (the caller has confirmed).
    pub fn delete_channel(&self, c: &ChannelRow) -> Result<ChannelImpact, ManageError> {
        let impact = self.channel_impact(c)?;
        if let Some(b) = impact.blockers.first() {
            return refuse(b.clone());
        }
        if !self.store.delete_channel(&c.id)? {
            return refuse("no such channel");
        }
        Ok(impact)
    }

    /// Give topic channel `c` a new slug.
    pub fn rename_channel_slug(&self, c: &ChannelRow, slug: &str) -> Result<(), ManageError> {
        let slug = slug.trim().trim_start_matches('#');
        tern_core::channels::check_slug(slug).map_err(ManageError::Refused)?;
        if c.kind != "topic" {
            return refuse("Home and DMs keep their names: rename the agent instead");
        }
        if slug != c.slug
            && self
                .store
                .channel(slug)
                .map_err(anyhow::Error::from)?
                .is_some()
        {
            return refuse(format!("there is already a channel #{slug}"));
        }
        self.store
            .set_channel_slug(&c.id, slug)
            .context("renaming the channel")?;
        Ok(())
    }
}

/// Ask on the terminal for `word` to be typed; false if it isn't. Refuses
/// to guess without a terminal: scripts pass `--yes`.
pub fn confirm_cli(word: &str) -> anyhow::Result<bool> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("not deleting without confirmation; pass --yes to skip the prompt");
    }
    print!("Type \"{word}\" to confirm: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().trim_start_matches(['@', '#']) == word)
}
