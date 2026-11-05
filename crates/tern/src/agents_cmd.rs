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

use crate::cli::{AgentsCommand, MemoryAction};
use crate::runtime::{NewAgent, Runtime};

pub fn run(home: &TernHome, command: Option<AgentsCommand>) -> anyhow::Result<ExitCode> {
    let rt = Runtime::open(home, Arc::new(AutoDeny))?;
    match command.unwrap_or(AgentsCommand::List { all: false }) {
        AgentsCommand::List { all } => list(&rt, all)?,
        AgentsCommand::Templates => templates(),
        AgentsCommand::New {
            name,
            slug,
            template,
            memory,
            autonomy,
        } => {
            let autonomy = autonomy.as_deref().map(parse_autonomy).transpose()?;
            let a = rt.create_agent(NewAgent {
                name,
                slug,
                template,
                memory,
                autonomy,
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
        AgentsCommand::Telegram { agent, secret } => {
            let a = find(&rt, &agent)?;
            if a.is_default() {
                bail!("the default agent's bot is [gateway.telegram] in config.toml");
            }
            let value = match secret.as_str() {
                "none" => None,
                name => {
                    tern_core::secrets::check_name(name)?;
                    if rt
                        .config
                        .gateway
                        .telegram
                        .as_ref()
                        .is_some_and(|g| g.bot_token_secret == name)
                    {
                        bail!("{name} is the main bot's token; an agent's bot needs its own");
                    }
                    for other in rt.store.agents(true)? {
                        if other.id != a.id
                            && tern_core::agents::AgentTelegram::parse(other.telegram.as_deref())
                                .is_some_and(|o| o.bot_token_secret == name)
                        {
                            bail!("@{} already uses {name}", other.slug);
                        }
                    }
                    Some(serde_json::to_string(&tern_core::agents::AgentTelegram {
                        bot_token_secret: name.to_owned(),
                        api_url: None,
                    })?)
                }
            };
            let set = value.is_some();
            rt.store.update_agent(
                &a.id,
                &AgentChange {
                    telegram: Some(value),
                    ..AgentChange::default()
                },
            )?;
            if set {
                println!(
                    "@{} gets its own Telegram bot from the token in secret {secret}; set it with `tern secrets set {secret}`, then restart the daemon. Pair once with `tern gateway pair`: it works on every bot.",
                    a.slug
                );
            } else {
                println!(
                    "@{} has no bot of its own; restart the daemon to apply",
                    a.slug
                );
            }
        }
        AgentsCommand::Autonomy { agent, level } => {
            let a = find(&rt, &agent)?;
            let ceiling = match level.as_str() {
                "none" | "default" => None,
                l => Some(parse_autonomy(l)?),
            };
            rt.store.update_agent(
                &a.id,
                &AgentChange {
                    autonomy: Some(ceiling.map(|c| c.as_str().to_owned())),
                    ..AgentChange::default()
                },
            )?;
            match ceiling {
                Some(c) => println!("@{}'s runs are capped at autonomy {c}", a.slug),
                None => println!("@{} has no autonomy ceiling", a.slug),
            }
        }
        AgentsCommand::Edit { agent } => edit(&rt, &agent)?,
        AgentsCommand::Memory { agent, action } => return memory(&rt, &agent, action),
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

fn parse_autonomy(s: &str) -> anyhow::Result<tern_core::approval::Autonomy> {
    tern_core::approval::Autonomy::parse(s).with_context(|| {
        format!("autonomy must be readonly, supervised, autonomous or yolo, not {s:?}")
    })
}

/// The starter catalog, grouped by category.
fn templates() {
    let all = tern_core::agents::templates();
    let mut seen: Vec<&str> = Vec::new();
    for t in &all {
        if !seen.contains(&t.category.as_str()) {
            seen.push(&t.category);
        }
    }
    for category in seen {
        println!("{category}");
        for t in all.iter().filter(|t| t.category == category) {
            let cap = t.autonomy.map_or("", |a| a.as_str());
            println!("  {:<18} {:<11} {}", t.id, cap, t.description);
        }
    }
    println!("\ntern agents new <name> --template <id>");
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
        let bot = if a.telegram.is_some() {
            "  own bot"
        } else {
            ""
        };
        let cap = a
            .autonomy
            .as_deref()
            .map(|c| format!("  max {c}"))
            .unwrap_or_default();
        println!(
            "@{:<16} {:<16} {:<8} memory  {model}  {routines} routine(s){cap}{bot}{archived}",
            a.slug, a.name, a.memory
        );
    }
    Ok(())
}

/// `tern agents memory`: view and fix what an agent remembers.
fn memory(rt: &Runtime, agent: &str, action: Option<MemoryAction>) -> anyhow::Result<ExitCode> {
    // The default agent's memory is the shared pool.
    let (label, ns) = match agent.trim_start_matches('@') {
        "shared" => ("shared".to_owned(), None),
        other => {
            let a = find(rt, other)?;
            if a.is_default() {
                ("shared".to_owned(), None)
            } else {
                (format!("@{}", a.slug), Some(a.id))
            }
        }
    };
    let scope = ns.as_deref().unwrap_or("shared");
    match action.unwrap_or(MemoryAction::List) {
        MemoryAction::List => {
            let rows = rt.store.list_memories(Some(scope), None, 500)?;
            if rows.is_empty() {
                println!("{label} remembers nothing yet");
            }
            for m in rows {
                let when = chrono::DateTime::from_timestamp_millis(m.created_at)
                    .map(|d| {
                        d.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d")
                            .to_string()
                    })
                    .unwrap_or_default();
                let tag = match m.source.as_str() {
                    "unverified" => "  [unverified]",
                    "user" => "  [yours]",
                    _ => "",
                };
                println!(
                    "#{:<5} {when}{tag}\n       {}",
                    m.id,
                    m.content.replace('\n', "\n       ")
                );
            }
        }
        MemoryAction::Edit { id, text } => {
            check_owned(rt, id, scope)?;
            let max = rt.config.memory.entry_max_chars;
            if text.trim().is_empty() || text.chars().count() > max {
                bail!("a memory is 1 to {max} characters");
            }
            rt.store.edit_memory(id, text.trim(), None)?;
            println!("rewrote memory #{id}; it is yours now");
        }
        MemoryAction::Rm { id } => {
            check_owned(rt, id, scope)?;
            rt.store.delete_memory(id)?;
            println!("deleted memory #{id}");
        }
        MemoryAction::Clear { yes } => {
            let (n, _) = rt.store.memory_usage(ns.as_deref())?;
            if n == 0 {
                println!("{label} remembers nothing");
                return Ok(ExitCode::SUCCESS);
            }
            println!("This deletes all {n} memories of {label}.");
            if !yes && !crate::manage::confirm_cli("clear")? {
                println!("left as it was");
                return Ok(ExitCode::FAILURE);
            }
            let n = rt.store.clear_memories(ns.as_deref())?;
            println!("cleared {n} memories of {label}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Memory `id` is in namespace `scope` (`shared` or an agent id).
fn check_owned(rt: &Runtime, id: i64, scope: &str) -> anyhow::Result<()> {
    let m = rt
        .store
        .memory(id)?
        .with_context(|| format!("no memory #{id}"))?;
    if m.agent_id.as_deref().unwrap_or("shared") != scope {
        bail!("memory #{id} is not in that agent's memory");
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
    let a = rt.rename_agent(&a, name.as_deref(), slug.as_deref())?;
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
    if let Some(secret) = impact.telegram_bot {
        println!(
            "its Telegram bot is dropped (the secret {secret} is yours to keep or remove); a \
             running daemon stops polling it at its next restart"
        );
    }
    Ok(ExitCode::SUCCESS)
}
