//! `tern channels`: list, create, edit, archive and delete topic channels.
//! Works on the database directly, so it needs no daemon; a running one
//! shows the change after its next refresh. Agents' DMs belong to their
//! agents (`tern agents`).

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use tern_core::TernHome;
use tern_core::approval::AutoDeny;
use tern_store::ChannelRow;

use crate::cli::ChannelsCommand;
use crate::runtime::Runtime;

pub fn run(home: &TernHome, command: Option<ChannelsCommand>) -> anyhow::Result<ExitCode> {
    let rt = Runtime::open(home, Arc::new(AutoDeny))?;
    match command.unwrap_or(ChannelsCommand::List { all: false }) {
        ChannelsCommand::List { all } => list(&rt, all)?,
        ChannelsCommand::New {
            slug,
            title,
            instructions,
        } => new(&rt, &slug, title, instructions)?,
        ChannelsCommand::Edit {
            channel,
            slug,
            title,
            instructions,
        } => edit(&rt, &channel, slug, title, instructions)?,
        ChannelsCommand::Archive { channel, undo } => {
            let c = find(&rt, &channel)?;
            if c.kind == "main" {
                bail!(
                    "{}",
                    if c.id == tern_store::HOME {
                        "Home can't be archived"
                    } else {
                        "an agent's DM is archived with its agent: `tern agents archive`"
                    }
                );
            }
            rt.store.update_channel(&c.id, None, None, Some(!undo))?;
            if undo {
                println!("#{} is back", c.slug);
            } else {
                println!("archived #{}: hidden, nothing deleted", c.slug);
            }
        }
        ChannelsCommand::Delete { channel, yes } => return delete(&rt, &channel, yes),
    }
    Ok(ExitCode::SUCCESS)
}

fn find(rt: &Runtime, channel: &str) -> anyhow::Result<ChannelRow> {
    rt.store
        .channel(channel.trim_start_matches('#'))?
        .with_context(|| format!("no channel {channel:?}; `tern channels --all` lists them"))
}

fn list(rt: &Runtime, all: bool) -> anyhow::Result<()> {
    for c in rt.store.channels(all)? {
        let name = match c.kind.as_str() {
            "topic" => format!("#{}", c.slug),
            _ if c.id == tern_store::HOME => "Home".to_owned(),
            _ => format!("DM @{}", c.title),
        };
        let archived = if c.archived_at.is_some() {
            "  [archived]"
        } else {
            ""
        };
        println!("{name:<24} {}{archived}", c.title);
    }
    Ok(())
}

fn new(
    rt: &Runtime,
    slug: &str,
    title: Option<String>,
    instructions: Option<String>,
) -> anyhow::Result<()> {
    let slug = slug.trim().trim_start_matches('#');
    tern_core::channels::check_slug(slug).map_err(anyhow::Error::msg)?;
    if rt.store.channel(slug)?.is_some() {
        bail!("there is already a channel #{slug}");
    }
    let c = rt.store.create_channel(&ChannelRow {
        id: tern_core::agent::new_id(),
        slug: slug.to_owned(),
        title: title
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| slug.to_owned()),
        kind: "topic".into(),
        instructions: instructions.filter(|i| !i.trim().is_empty()),
        created_by: "user".into(),
        ..ChannelRow::default()
    })?;
    println!("created #{}", c.slug);
    Ok(())
}

fn edit(
    rt: &Runtime,
    channel: &str,
    slug: Option<String>,
    title: Option<String>,
    instructions: Option<String>,
) -> anyhow::Result<()> {
    let c = find(rt, channel)?;
    if slug.is_none() && title.is_none() && instructions.is_none() {
        bail!("nothing to change: pass --slug, --title or --instructions");
    }
    if c.kind != "topic" {
        bail!("only topic channels can be edited here; a DM follows its agent (`tern agents rename`)");
    }
    let title = title.map(|t| t.trim().to_owned());
    if title.as_deref() == Some("") {
        bail!("title is empty");
    }
    if let Some(s) = &slug {
        rt.rename_channel_slug(&c, s)?;
    }
    let instructions = instructions.map(|i| i.trim().to_owned());
    rt.store.update_channel(
        &c.id,
        title.as_deref(),
        instructions
            .as_deref()
            .map(|i| Some(i).filter(|i| !i.is_empty())),
        None,
    )?;
    let c = find(rt, &c.id)?;
    println!("#{} ({})", c.slug, c.title);
    Ok(())
}

fn delete(rt: &Runtime, channel: &str, yes: bool) -> anyhow::Result<ExitCode> {
    let c = find(rt, channel)?;
    let impact = rt.channel_impact(&c)?;
    println!("{}", impact.describe());
    if !impact.blockers.is_empty() {
        return Ok(ExitCode::FAILURE);
    }
    if !yes && !crate::manage::confirm_cli(&c.slug)? {
        println!("Nothing was deleted.");
        return Ok(ExitCode::FAILURE);
    }
    rt.delete_channel(&c)?;
    println!("deleted #{}; its conversations stay in History", c.slug);
    Ok(ExitCode::SUCCESS)
}
