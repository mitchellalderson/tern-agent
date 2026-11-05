//! `tern workspace`: the folders agents can reach. Works on the database
//! directly, so it needs no daemon; a running one picks the change up at the
//! next run.

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use tern_core::TernHome;
use tern_core::approval::AutoDeny;
use tern_store::{Bind, DEFAULT_WORKSPACE, WorkspaceRow};

use crate::cli::{WorkspaceCommand, WorkspaceRootsCommand, WorkspaceTarget};
use crate::runtime::Runtime;
use crate::workspaces::Refused;

pub fn run(home: &TernHome, command: Option<WorkspaceCommand>) -> anyhow::Result<ExitCode> {
    let rt = Runtime::open(home, Arc::new(AutoDeny))?;
    match command.unwrap_or(WorkspaceCommand::List { all: false }) {
        WorkspaceCommand::List { all } => list(&rt, all)?,
        WorkspaceCommand::Add { path, name } => {
            let (row, verdict) = add(&rt, &path, name.as_deref())?;
            println!("{}  {}  {}", row.id, row.name, row.path);
            for w in &verdict.warnings {
                println!("  note: {w}");
            }
        }
        WorkspaceCommand::Get { target } => get(&rt, &target)?,
        WorkspaceCommand::Set { workspace, target } => {
            let ws = rt.resolve_ws_ref(&workspace).map_err(show)?;
            let t = bind_target(&rt, &target)?;
            rt.bind_workspace(t.bind(), Some(&ws))?;
            println!("{} now uses {} ({})", t.label(), ws.name, ws.path);
            if matches!(t, Target::Session(_)) {
                println!(
                    "  (a run in this conversation will start in the new folder; an older transcript may mention old paths)"
                );
            }
        }
        WorkspaceCommand::Clear { target } => {
            let t = bind_target(&rt, &target)?;
            rt.bind_workspace(t.bind(), None)?;
            println!("{} inherits its folder again", t.label());
        }
        WorkspaceCommand::Check { workspace } => return check(&rt, workspace.as_deref()),
        WorkspaceCommand::Remove {
            workspace,
            reassign_to,
        } => remove(&rt, &workspace, reassign_to.as_deref())?,
        WorkspaceCommand::Roots { workspace, command } => roots(&rt, &workspace, command)?,
    }
    Ok(ExitCode::SUCCESS)
}

/// A refused folder shows its reason without the error chain.
fn show(e: anyhow::Error) -> anyhow::Error {
    match e.downcast::<Refused>() {
        Ok(r) => anyhow::anyhow!("{r}"),
        Err(e) => e,
    }
}

fn add(
    rt: &Runtime,
    path: &str,
    name: Option<&str>,
) -> anyhow::Result<(WorkspaceRow, tern_core::workspace::validate::Verdict)> {
    rt.add_workspace(path, name).map_err(show)
}

fn find(rt: &Runtime, r: &str) -> anyhow::Result<WorkspaceRow> {
    match rt.store.workspace(r)? {
        Some(w) => Ok(w),
        None => rt
            .store
            .workspace_by_path(r)?
            .with_context(|| format!("no workspace {r:?}; `tern workspace list` shows them")),
    }
}

fn list(rt: &Runtime, all: bool) -> anyhow::Result<()> {
    for w in rt.store.workspaces(all)? {
        let used = rt.store.workspace_use(&w.id)?;
        let mut what = Vec::new();
        for c in &used.channels {
            what.push(format!("#{c}"));
        }
        for a in &used.agents {
            what.push(format!("@{a}"));
        }
        if used.sessions > 0 {
            what.push(format!("{} topic(s)", used.sessions));
        }
        let status = if w.status == "ok" {
            String::new()
        } else {
            format!("  [{}]", w.status)
        };
        let archived = if w.archived_at.is_some() {
            "  [archived]"
        } else {
            ""
        };
        println!("{:<34} {:<18} {}{status}{archived}", w.id, w.name, w.path);
        if !what.is_empty() {
            println!("{:<34} used by {}", "", what.join(", "));
        }
    }
    Ok(())
}

enum Target {
    Session(String),
    Channel(String, String),
    Agent(String, String),
}

impl Target {
    fn bind(&self) -> Bind<'_> {
        match self {
            Target::Session(i) => Bind::Session(i),
            Target::Channel(i, _) => Bind::Channel(i),
            Target::Agent(i, _) => Bind::Agent(i),
        }
    }

    fn label(&self) -> String {
        match self {
            Target::Session(i) => format!("conversation {i}"),
            Target::Channel(_, n) => format!("#{n}"),
            Target::Agent(_, n) => format!("@{n}"),
        }
    }
}

fn bind_target(rt: &Runtime, t: &WorkspaceTarget) -> anyhow::Result<Target> {
    match (&t.session, &t.channel, &t.agent) {
        (Some(s), None, None) => {
            rt.store
                .session(s)?
                .with_context(|| format!("no session {s:?}"))?;
            Ok(Target::Session(s.clone()))
        }
        (None, Some(c), None) => {
            let c = rt
                .store
                .channel(c.trim_start_matches('#'))?
                .with_context(|| format!("no channel {c:?}; `tern channels --all` lists them"))?;
            Ok(Target::Channel(c.id, c.slug))
        }
        (None, None, Some(a)) => {
            let a = rt
                .store
                .agent(a.trim_start_matches('@'))?
                .with_context(|| format!("no agent {a:?}; `tern agents` lists them"))?;
            Ok(Target::Agent(a.id, a.name))
        }
        _ => bail!("name one of --session, --channel or --agent"),
    }
}

fn describe(rt: &Runtime, w: &WorkspaceRow) -> anyhow::Result<()> {
    println!("{}  {}", w.name, w.path);
    if w.status != "ok" {
        println!(
            "  unavailable ({}): {}",
            w.status,
            w.status_detail.as_deref().unwrap_or("")
        );
    }
    for r in rt.store.workspace_roots(&w.id)? {
        println!(
            "  read-only {}: {}",
            if r.is_file { "file" } else { "folder" },
            r.path
        );
    }
    Ok(())
}

fn get(rt: &Runtime, t: &WorkspaceTarget) -> anyhow::Result<()> {
    if t.session.is_none() && t.channel.is_none() && t.agent.is_none() {
        let w = rt
            .store
            .workspace(DEFAULT_WORKSPACE)?
            .context("the default workspace isn't set up")?;
        println!("default folder:");
        return describe(rt, &w);
    }
    let target = bind_target(rt, t)?;
    let own = rt.store.bound_workspace(target.bind())?;
    let (effective, source) = match &target {
        Target::Session(s) => {
            let (w, src) = rt.effective_workspace(s)?;
            (w, src.as_str().to_owned())
        }
        _ => match own.as_deref() {
            Some(id) => (find(rt, id)?, "its own binding".to_owned()),
            None => {
                let agent_ws = match &target {
                    Target::Channel(id, _) => rt
                        .store
                        .channel(id)?
                        .and_then(|c| c.agent_id)
                        .and_then(|a| rt.store.bound_workspace(Bind::Agent(&a)).ok().flatten()),
                    _ => None,
                };
                match agent_ws {
                    Some(id) => (find(rt, &id)?, "its agent".to_owned()),
                    None => (find(rt, DEFAULT_WORKSPACE)?, "the default".to_owned()),
                }
            }
        },
    };
    println!("{} uses (from {source}):", target.label());
    describe(rt, &effective)
}

fn check(rt: &Runtime, which: Option<&str>) -> anyhow::Result<ExitCode> {
    let rows = match which {
        Some(w) => vec![find(rt, w)?],
        None => rt.store.workspaces(false)?,
    };
    let mut bad = false;
    for w in rows {
        match rt.check_workspace(&w.id) {
            Ok(f) => println!("ok        {}  {}", w.name, f.root.display()),
            Err(e) => {
                bad = true;
                println!("{:<9} {}  {}", e.status, w.name, e.detail);
            }
        }
    }
    Ok(if bad {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn remove(rt: &Runtime, workspace: &str, reassign_to: Option<&str>) -> anyhow::Result<()> {
    let w = find(rt, workspace)?;
    if w.id == DEFAULT_WORKSPACE {
        bail!("the default workspace can't be removed");
    }
    let used = rt.store.workspace_use(&w.id)?;
    if !used.is_empty() {
        let Some(to) = reassign_to else {
            bail!(
                "{} is in use (channels: {}; agents: {}; routines: {}; {} topic(s)); pass --reassign-to <workspace> to move them",
                w.name,
                used.channels.join(", "),
                used.agents.join(", "),
                used.schedules.join(", "),
                used.sessions
            );
        };
        let target = rt.resolve_ws_ref(to).map_err(show)?;
        rt.store.reassign_workspace(&w.id, &target.id)?;
    }
    rt.store.archive_workspace(&w.id)?;
    println!("removed {}: the folder and its files are untouched", w.name);
    Ok(())
}

fn roots(
    rt: &Runtime,
    workspace: &str,
    command: Option<WorkspaceRootsCommand>,
) -> anyhow::Result<()> {
    let w = find(rt, workspace)?;
    match command.unwrap_or(WorkspaceRootsCommand::List) {
        WorkspaceRootsCommand::List => {
            for r in rt.store.workspace_roots(&w.id)? {
                println!(
                    "{}  {}",
                    if r.is_file { "file  " } else { "folder" },
                    r.path
                );
            }
        }
        WorkspaceRootsCommand::Add { path } => {
            let (r, v) = rt.add_read_root(&w.id, &path).map_err(show)?;
            println!("{} can now read {} (never change it)", w.name, r.path);
            for n in &v.warnings {
                println!("  note: {n}");
            }
        }
        WorkspaceRootsCommand::Remove { path } => {
            let canon = std::fs::canonicalize(&path)
                .ok()
                .and_then(|p| p.to_str().map(str::to_owned))
                .unwrap_or_else(|| path.clone());
            if !(rt.store.remove_workspace_root(&w.id, &canon)?
                || rt.store.remove_workspace_root(&w.id, &path)?)
            {
                bail!("{path} isn't a read-only folder of {}", w.name);
            }
            println!("removed");
        }
    }
    Ok(())
}
