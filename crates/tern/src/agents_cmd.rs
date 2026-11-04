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
