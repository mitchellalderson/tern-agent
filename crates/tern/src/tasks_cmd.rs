//! `tern tasks`: the Tasks view in a terminal (tooling spec §8.2), from
//! the daemon, which alone knows what is running and what waits on you.

use std::process::ExitCode;

use anyhow::{Context, bail};
use tern_core::TernHome;

use crate::daemon::api_tasks::{SECTIONS, Task, Tasks};
use crate::lifecycle::{block_on, socket_client};

pub fn run(home: &TernHome, json: bool) -> anyhow::Result<ExitCode> {
    let http = socket_client(home)
        .context("tern isn't running; start it with `tern start` to see its tasks")?;
    let tasks: Tasks = block_on(async move {
        let resp = http
            .get("http://tern/v1/tasks")
            .send()
            .await
            .context("asking tern")?;
        if !resp.status().is_success() {
            bail!("tern answered {}", resp.status());
        }
        Ok::<_, anyhow::Error>(resp.json().await?)
    })??;
    if json {
        println!("{}", serde_json::to_string_pretty(&tasks)?);
    } else {
        print!("{}", render(&tasks, tern_store::now_ms()));
    }
    Ok(ExitCode::SUCCESS)
}

fn heading(section: &str) -> &'static str {
    match section {
        "needs_you" => "Needs you",
        "running" => "Running",
        "queued" => "Queued",
        "scheduled" => "Scheduled",
        _ => "Failed recently",
    }
}

/// `in 3h`, `5m ago`.
fn relative(at: i64, now: i64) -> String {
    let (d, future) = if at >= now {
        (at - now, true)
    } else {
        (now - at, false)
    };
    let mins = d / 60_000;
    let s = match mins {
        0 => {
            return if future {
                "now".into()
            } else {
                "just now".into()
            };
        }
        m if m < 60 => format!("{m}m"),
        m if m < 48 * 60 => format!("{}h", m / 60),
        m => format!("{}d", m / (24 * 60)),
    };
    if future {
        format!("in {s}")
    } else {
        format!("{s} ago")
    }
}

fn line(t: &Task, now: i64) -> String {
    let mut parts = vec![t.title.clone()];
    if let Some(d) = &t.detail {
        parts.push(d.lines().next().unwrap_or("").to_owned());
    }
    if let Some(at) = t.at {
        parts.push(relative(at, now));
    }
    let place = match (&t.agent_id, &t.channel_id) {
        (Some(a), Some(c)) => format!("{a} in {c}"),
        (Some(a), None) => a.clone(),
        (None, Some(c)) => c.clone(),
        (None, None) => String::new(),
    };
    if !place.is_empty() {
        parts.push(place);
    }
    let tainted = if t.tainted { " [tainted]" } else { "" };
    format!("{:<15} {}{tainted}", t.kind, parts.join(" · "))
}

pub fn render(tasks: &Tasks, now: i64) -> String {
    let mut out = String::new();
    for section in SECTIONS {
        let items: Vec<&Task> = tasks
            .items
            .iter()
            .filter(|t| t.section == section)
            .collect();
        if items.is_empty() {
            continue;
        }
        out.push_str(&format!("{} ({})\n", heading(section), items.len()));
        // Sub-agents under their parent, when it is listed.
        let listed = |id: &str| items.iter().any(|t| t.run_id.as_deref() == Some(id));
        for t in items
            .iter()
            .filter(|t| !t.parent_run_id.as_deref().is_some_and(listed))
        {
            out.push_str(&format!("  {}\n", line(t, now)));
            for kid in items
                .iter()
                .filter(|k| k.parent_run_id.is_some() && k.parent_run_id == t.run_id)
            {
                out.push_str(&format!("    ↳ {}\n", line(kid, now)));
            }
        }
    }
    if out.is_empty() {
        out.push_str("Nothing running, queued, scheduled or waiting on you.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_render_in_order_with_sub_agents_nested() {
        let t = |section: &str, kind: &str, title: &str| Task {
            section: section.into(),
            kind: kind.into(),
            title: title.into(),
            ..Task::default()
        };
        let tasks = Tasks {
            items: vec![
                Task {
                    run_id: Some("r1".into()),
                    at: Some(0),
                    ..t("running", "run", "Morning brief")
                },
                Task {
                    run_id: Some("r2".into()),
                    parent_run_id: Some("r1".into()),
                    ..t("running", "run", "sub-agent research")
                },
                Task {
                    tainted: true,
                    agent_id: Some("tern".into()),
                    channel_id: Some("home".into()),
                    ..t("needs_you", "outbox", "write · send to bob")
                },
            ],
            counts: Default::default(),
        };
        let s = render(&tasks, 120_000);
        assert_eq!(
            s,
            "Needs you (1)\n  outbox          write · send to bob · tern in home [tainted]\n\
             Running (2)\n  run             Morning brief · 2m ago\n    ↳ run             sub-agent research\n"
        );
        assert!(render(&Tasks::default(), 0).starts_with("Nothing running"));
        assert_eq!(relative(3 * 3_600_000, 0), "in 3h");
    }
}
