//! `tern agents`: list, create, edit and archive agent profiles
//! (docs/agent-profiles-design.md §7.3). Works on the database and persona
//! files directly, so it needs no daemon; a running one picks the changes
//! up at the agent's next run. The desktop app has the full editor.

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use tern_core::TernHome;
use tern_core::approval::AutoDeny;
use tern_store::AgentChange;

use crate::cli::AgentsCommand;
use crate::runtime::{NewAgent, Runtime};

pub fn run(home: &TernHome, command: Option<AgentsCommand>) -> anyhow::Result<ExitCode> {
    let rt = Runtime::open(home, Arc::new(AutoDeny))?;
    match command.unwrap_or(AgentsCommand::List { all: false }) {
        AgentsCommand::List { all } => list(&rt, all)?,
        AgentsCommand::New {
            name,
            slug,
            template,
            memory,
        } => {
            let a = rt.create_agent(NewAgent {
                name,
                slug,
                template,
                memory,
                ..NewAgent::default()
            })?;
            println!(
                "created @{} ({}): its DM is #{}; edit its persona with `tern agents edit {}`",
                a.slug,
                a.name,
                tern_store::dm_slug(&a.slug),
                a.slug
            );
            println!(
                "persona: {}",
                tern_core::agents::persona_path(home, &a.id).display()
            );
        }
        AgentsCommand::Edit { agent } => edit(&rt, &agent)?,
        AgentsCommand::Rename { agent, name, slug } => rename(&rt, &agent, name, slug)?,
        AgentsCommand::Delete {
            agent,
            reassign_to,
            delete_routines,
            yes,
        } => return delete(&rt, &agent, reassign_to, delete_routines, yes),
        AgentsCommand::Archive { agent, undo } => {
            let a = find(&rt, &agent)?;
            if a.is_default() {
                bail!("the default agent can't be archived");
            }
            rt.store.update_agent(
                &a.id,
                &AgentChange {
                    archived: Some(!undo),
                    ..AgentChange::default()
                },
            )?;
            if undo {
                println!(
                    "@{} is back; its routines stay paused until you enable them",
                    a.slug
                );
            } else {
                println!(
                    "archived @{}; its routines are paused and its DM is hidden",
                    a.slug
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn find(rt: &Runtime, agent: &str) -> anyhow::Result<tern_store::AgentRow> {
    rt.store
        .agent(agent.trim_start_matches('@'))?
        .with_context(|| format!("no agent {agent:?}; `tern agents` lists them"))
}

fn list(rt: &Runtime, all: bool) -> anyhow::Result<()> {
    for a in rt.store.agents(all)? {
        let model = match (a.is_default(), a.model.as_deref()) {
            (true, _) | (false, None) => "default model".to_owned(),
            (false, Some(m)) => tern_core::config::ModelChain::from_stored(m)
                .map(|c| c.to_string())
                .unwrap_or_else(|| m.to_owned()),
        };
        let routines = rt.store.agent_schedule_ids(&a.id)?.len();
        let archived = if a.archived_at.is_some() {
            "  [archived]"
        } else {
            ""
        };
        println!(
            "@{:<16} {:<16} {:<8} memory  {model}  {routines} routine(s){archived}",
            a.slug, a.name, a.memory
        );
    }
    Ok(())
}

/// Open the agent's persona in `$VISUAL` / `$EDITOR`.
fn edit(rt: &Runtime, agent: &str) -> anyhow::Result<()> {
    let a = find(rt, agent)?;
    let path = tern_core::agents::persona_path(&rt.home, &a.id);
    if !path.exists() {
        rt.write_persona(&a.id, tern_core::defaults::PERSONA_MD)?;
    }
    let editor = std::env::var("VISUAL")
        .ok()
        .or_else(|| std::env::var("EDITOR").ok())
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "vi".into());
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let status = std::process::Command::new(program)
        .args(parts)
        .arg(&path)
        .status()
        .with_context(|| format!("starting {editor}"))?;
    if !status.success() {
        bail!("{editor} exited with {status}");
    }
    rt.store.update_agent(&a.id, &AgentChange::default())?;
    println!(
        "saved {}; it applies from {}'s next run",
        path.display(),
        a.name
    );
    Ok(())
}

/// `tern agents rename <agent> [name] [--slug s]`.
fn rename(
    rt: &Runtime,
    agent: &str,
    name: Option<String>,
    slug: Option<String>,
) -> anyhow::Result<()> {
    let a = find(rt, agent)?;
    let name = name.map(|n| n.trim().to_owned());
    if name.is_none() && slug.is_none() {
        bail!("give a new name, or --slug for a new @name");
    }
    if name.as_deref() == Some("") {
        bail!("name is empty");
    }
    let slug = slug.map(|s| s.trim().trim_start_matches('@').to_owned());
    if let Some(s) = &slug {
        if a.is_default() && s != tern_store::DEFAULT_AGENT {
            bail!("the default agent's @name stays `tern`; rename its display name instead");
        }
        tern_core::agents::check_slug(s).map_err(anyhow::Error::msg)?;
        if rt.store.agent(s)?.is_some_and(|o| o.id != a.id) {
            bail!("there is already an agent @{s}");
        }
        if rt
            .store
            .channel(&tern_store::dm_slug(s))?
            .is_some_and(|c| c.agent_id.as_deref() != Some(a.id.as_str()))
        {
            bail!("the channel #{} is taken", tern_store::dm_slug(s));
        }
    }
    rt.store.update_agent(
        &a.id,
        &AgentChange {
            name,
            slug,
            ..AgentChange::default()
        },
    )?;
    let a = find(rt, &a.id)?;
    println!("@{} is now {} (id {} is unchanged)", a.slug, a.name, a.id);
    Ok(())
}

/// `tern agents delete`: show what it touches, ask, then delete.
fn delete(
    rt: &Runtime,
    agent: &str,
    reassign_to: Option<String>,
    delete_routines: bool,
    yes: bool,
) -> anyhow::Result<ExitCode> {
    let a = find(rt, agent)?;
    let impact = rt.agent_impact(&a)?;
    println!("{}", impact.describe());
    if !impact.blockers.is_empty() {
        return Ok(ExitCode::FAILURE);
    }
    if !yes && !crate::manage::confirm_cli(&a.slug)? {
        println!("Nothing was deleted.");
        return Ok(ExitCode::FAILURE);
    }
    rt.delete_agent(
        &a,
        &crate::manage::DeleteAgent {
            reassign_to,
            delete_routines,
        },
    )?;
    println!("deleted @{}; its history stays readable", a.slug);
    Ok(ExitCode::SUCCESS)
}
