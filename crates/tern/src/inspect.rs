//! `tern runs ...` and `tern sessions ...`: read-only views of the database.

use anyhow::Context;
use serde_json::{Value, json};
use tern_core::TernHome;
use tern_core::types::{AssistantTurn, ContentBlock};
use tern_store::{StepRow, Store};

fn open(home: &TernHome) -> anyhow::Result<Option<Store>> {
    let path = home.db_path();
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(
        Store::open(&path).with_context(|| format!("opening {}", path.display()))?,
    ))
}

fn time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "-".into())
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

pub fn runs_list(home: &TernHome) -> anyhow::Result<()> {
    let Some(store) = open(home)? else {
        println!("no runs yet");
        return Ok(());
    };
    // Sub-agents are listed under their parent by `runs show`.
    for r in store
        .recent_runs(60)?
        .into_iter()
        .filter(|r| r.parent_run_id.is_none())
        .take(20)
    {
        let kids = store.children(&r.id)?.len();
        let kids = match kids {
            0 => String::new(),
            1 => "  (+1 sub-agent)".into(),
            n => format!("  (+{n} sub-agents)"),
        };
        let tainted = if r.taint.is_some() { "  tainted" } else { "" };
        println!(
            "{}  {:<11} {}  {}{kids}{tainted}",
            r.id,
            r.status,
            time(r.started_at),
            r.model.as_deref().unwrap_or("-")
        );
    }
    Ok(())
}

pub fn runs_show(home: &TernHome, run_id: &str) -> anyhow::Result<()> {
    let store = open(home)?.context("no database yet")?;
    let run = store
        .run(run_id)?
        .with_context(|| format!("run {run_id} not found"))?;
    println!("run      {}", run.id);
    println!("session  {}", run.session_id);
    println!("status   {}", run.status);
    println!("trigger  {}", run.trigger);
    println!("model    {}", run.model.as_deref().unwrap_or("-"));
    let steps = store.steps(run_id)?;
    let fallbacks = steps.iter().filter(|s| answered_by(s).is_some()).count();
    if fallbacks > 0 {
        println!("fallback {fallbacks} step(s) answered by a fallback model");
    }
    println!("started  {}", time(run.started_at));
    if let Some(f) = run.finished_at {
        println!("finished {}", time(f));
    }
    if let Some(e) = &run.error {
        println!("error    {e}");
    }
    if let Some(t) = &run.taint {
        println!("tainted  {t} (writes ask; tooling spec §5.3)");
    }
    if let Some(parent) = &run.parent_run_id {
        println!(
            "parent   {parent} (sub-agent {}, depth {})",
            run.label.as_deref().unwrap_or("?"),
            run.depth
        );
        if let Some(scope) = &run.tool_scope {
            let names: Vec<String> = serde_json::from_str(scope).unwrap_or_default();
            println!("tools    {}", names.join(", "));
        }
        let limit = |v: Option<String>| v.unwrap_or_else(|| "-".into());
        println!(
            "limits   {} iterations, {} tokens",
            limit(run.max_iterations.map(|n| n.to_string())),
            limit(run.budget_tokens.map(|n| n.to_string()))
        );
    }
    println!();
    for step in steps {
        let dur = step
            .finished_at
            .map(|f| format!("{:.1}s", (f - step.started_at) as f64 / 1000.0))
            .unwrap_or_else(|| "-".into());
        println!(
            "{:>3}  {:<10} {:<9} {:>6}  {}",
            step.idx,
            step.kind,
            step.status,
            dur,
            describe(&step)
        );
        // The sub-agent an agent_spawn step started, with its own children.
        if step.kind == "tool"
            && let Ok(call) = serde_json::from_str::<tern_core::types::ToolCall>(&step.input_json)
            && call.name == "agent_spawn"
            && let Some(child) = store.child_by_call(run_id, &call.id)?
        {
            print_tree(&store, &child, 1)?;
        }
    }
    Ok(())
}

/// What a code auto mode run changed since its snapshot (`tern runs changes`).
pub fn runs_changes(home: &TernHome, run_id: &str) -> anyhow::Result<()> {
    let run = run_for_checkpoint(home, run_id)?;
    let list = checkpoint_changes(home, &run)?;
    if list.is_empty() {
        println!("no changes since this run's snapshot (or it has none)");
    } else {
        println!("{} change(s) since this run's snapshot:", list.len());
        print_changes(&list, false);
        println!("`tern runs undo {run_id}` puts the folder back as the snapshot had it.");
    }
    Ok(())
}

/// Restore the folder to the run's snapshot (`tern runs undo`). Lists what it
/// would do first; `yes` does it. Nothing outside the folder and nothing in
/// the user's own `.git` is touched.
pub fn runs_undo(home: &TernHome, run_id: &str, yes: bool) -> anyhow::Result<()> {
    let run = run_for_checkpoint(home, run_id)?;
    let list = checkpoint_changes(home, &run)?;
    if list.is_empty() {
        println!("nothing to undo: no changes since this run's snapshot (or it has none)");
        return Ok(());
    }
    println!(
        "undoing run {run_id} in {} would:",
        run.workspace_root.as_deref().unwrap_or("?")
    );
    print_changes(&list, true);
    if !yes {
        println!("\nRun again with --yes to do this.");
        return Ok(());
    }
    let (id, root) = checkpoint_target(&run)?;
    let done = tern_core::checkpoint::restore(&home.checkpoints_dir(), &id, &root, run_id)
        .map_err(|e| anyhow::anyhow!(e.describe()))?;
    println!("restored: {} file(s) changed back", done.len());
    Ok(())
}

fn run_for_checkpoint(home: &TernHome, run_id: &str) -> anyhow::Result<tern_store::RunRow> {
    let store = open(home)?.context("no database yet")?;
    store
        .run(run_id)?
        .with_context(|| format!("run {run_id} not found"))
}

fn checkpoint_target(run: &tern_store::RunRow) -> anyhow::Result<(String, std::path::PathBuf)> {
    let root = run
        .workspace_root
        .as_deref()
        .context("this run has no workspace folder on record")?;
    Ok((
        run.workspace_id.clone().unwrap_or_else(|| "default".into()),
        std::path::PathBuf::from(root),
    ))
}

fn checkpoint_changes(
    home: &TernHome,
    run: &tern_store::RunRow,
) -> anyhow::Result<Vec<tern_core::checkpoint::Change>> {
    let (id, root) = checkpoint_target(run)?;
    tern_core::checkpoint::changes(&home.checkpoints_dir(), &id, &root, &run.id)
        .map_err(|e| anyhow::anyhow!(e.describe()))
}

/// One line per change; `plan` words it as what undoing would do.
fn print_changes(list: &[tern_core::checkpoint::Change], plan: bool) {
    use tern_core::checkpoint::ChangeKind;
    for c in list {
        if plan {
            let what = match c.kind {
                ChangeKind::Added => "remove (the run created it)",
                ChangeKind::Modified => "restore the earlier version of",
                ChangeKind::Deleted => "bring back",
            };
            println!("  {what} {}", c.path);
        } else {
            let mark = match c.kind {
                ChangeKind::Added => "+",
                ChangeKind::Modified => "~",
                ChangeKind::Deleted => "-",
            };
            println!("  {mark} {}", c.path);
        }
    }
}

/// `child` and its descendants, one line each, indented by depth.
fn print_tree(store: &Store, child: &tern_store::RunRow, level: usize) -> anyhow::Result<()> {
    println!(
        "{:indent$}└ {} {:<11} {}  {}{}",
        "",
        child.label.as_deref().unwrap_or("?"),
        child.status,
        child.id,
        child.model.as_deref().unwrap_or("-"),
        if child.taint.is_some() {
            "  tainted"
        } else {
            ""
        },
        indent = 5 + level * 2
    );
    for c in store.children(&child.id)? {
        print_tree(store, &c, level + 1)?;
    }
    Ok(())
}

/// The fallback model that answered `step`, when one did (T6a).
fn answered_by(step: &StepRow) -> Option<String> {
    let usage: Value = serde_json::from_str(step.usage_json.as_deref()?).ok()?;
    usage["model"].as_str().map(str::to_owned)
}

/// One-line description of a step from its stored input and output.
fn describe(step: &StepRow) -> String {
    if let Some(e) = &step.error {
        return format!("error: {}", one_line(e, 100));
    }
    if step.kind != "llm"
        && let Some(model) = answered_by(step)
    {
        return format!("{} (answered by {model})", describe_kind(step));
    }
    describe_kind(step)
}

fn describe_kind(step: &StepRow) -> String {
    let mut input: Value = serde_json::from_str(&step.input_json).unwrap_or(Value::Null);
    let output: Option<Value> = step
        .output_json
        .as_deref()
        .and_then(|o| serde_json::from_str(o).ok());
    // A script's tool call (docs/python-tool-spec.md §3.3): shown like a
    // tool step, marked as the script's.
    let mut kind = step.kind.as_str();
    if kind == "py_call" {
        if input["env"] == true {
            return format!("script environment (step {})", input["in"]);
        }
        let call = input["call"].take();
        input = json!({ "id": "", "name": call["name"], "input": call["input"] });
        kind = "tool";
    }
    match kind {
        "llm" => {
            let Some(turn) = output.and_then(|o| serde_json::from_value::<AssistantTurn>(o).ok())
            else {
                return format!("{} messages in context", input["messages"]);
            };
            let calls: Vec<String> = turn.message.tool_uses().map(|c| c.name).collect();
            let text = one_line(&turn.message.text(), 70);
            let usage = format!(
                "[{} in / {} out]",
                turn.usage.input_tokens, turn.usage.output_tokens
            );
            let line = match (text.is_empty(), calls.is_empty()) {
                (_, false) if text.is_empty() => format!("→ {} {usage}", calls.join(", ")),
                (_, false) => format!("{text:?} → {} {usage}", calls.join(", ")),
                _ => format!("{text:?} {usage}"),
            };
            match turn.fallback {
                Some(f) => format!("{line}\n{:34}answered by {} ({})", "", f.model, f.reason),
                None => line,
            }
        }
        "tool" | "approval" => {
            let call: Option<tern_core::types::ToolCall> = serde_json::from_value(input).ok();
            let summary = call
                .as_ref()
                .map(tern_core::agent::summarize)
                .unwrap_or_default();
            let result = match output {
                Some(Value::String(s)) => s,
                Some(o) => {
                    let mark = if o["untrusted"].as_bool() == Some(true) {
                        " [untrusted]"
                    } else {
                        ""
                    };
                    match (o["is_error"].as_bool(), o["content"].as_str()) {
                        (Some(true), Some(c)) => format!("✗ {}{mark}", one_line(c, 60)),
                        (_, Some(c)) => format!("✓ {}{mark}", one_line(c, 60)),
                        _ => o.to_string(),
                    }
                }
                None => "(no result)".into(),
            };
            format!("{} — {result}", one_line(&summary, 60))
        }
        "inbox" => {
            let out: Option<tern_core::subagent::InboxOutput> =
                output.and_then(|o| serde_json::from_value(o).ok());
            let wait = if input["wait"].as_bool() == Some(true) {
                "waited for sub-agents; "
            } else {
                ""
            };
            match out {
                Some(o) if o.mail.is_empty() => format!("{wait}no mail"),
                Some(o) => {
                    let items: Vec<String> = o
                        .mail
                        .iter()
                        .map(|m| {
                            let item = match (&m.agent, &m.status) {
                                (Some(a), Some(s)) => format!("{a} {s}"),
                                _ => m.kind.clone(),
                            };
                            if m.tainted {
                                format!("{item} [tainted]")
                            } else {
                                item
                            }
                        })
                        .collect();
                    format!("{wait}delivered: {}", items.join(", "))
                }
                None => format!("{wait}(no result)"),
            }
        }
        "compaction" => match output {
            Some(Value::String(s)) => format!(
                "summarized through message {}: {}",
                input["cover_through"],
                one_line(&s, 60)
            ),
            _ => format!("summarizing through message {}", input["cover_through"]),
        },
        _ => String::new(),
    }
}

pub fn sessions_list(home: &TernHome) -> anyhow::Result<()> {
    let Some(store) = open(home)? else {
        println!("no sessions yet");
        return Ok(());
    };
    for s in store.list_sessions(50)? {
        println!(
            "{}  {}  {:<8} {:>3} msgs  {}",
            s.session.id,
            time(s.session.updated_at),
            s.session.source,
            s.messages,
            one_line(s.session.title.as_deref().unwrap_or("(untitled)"), 60)
        );
    }
    Ok(())
}

pub fn sessions_show(home: &TernHome, id: &str) -> anyhow::Result<()> {
    let store = open(home)?.context("no database yet")?;
    let session = store
        .session(id)?
        .with_context(|| format!("session {id} not found"))?;
    println!(
        "session {} ({}, model {})",
        session.id,
        session.source,
        session.model.as_deref().unwrap_or("-")
    );
    if let Some(t) = &session.title {
        println!("title   {t}");
    }
    let mut last_run: Option<String> = None;
    for m in store.session_messages(id)? {
        if m.run_id != last_run {
            if let Some(run) = &m.run_id {
                println!("\n── run {run} ──");
            }
            last_run = m.run_id.clone();
        }
        let blocks: Vec<ContentBlock> = serde_json::from_str(&m.content_json).unwrap_or_default();
        let who = if m.summary_of.is_some() {
            "summary"
        } else {
            m.role.as_str()
        };
        for b in blocks {
            match b {
                ContentBlock::Text { text } if !text.trim().is_empty() => {
                    println!("\n[{who}] {}", text.trim());
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    println!("  → {name} {}", one_line(&input.to_string(), 100));
                }
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => {
                    let mark = if is_error { "✗" } else { "✓" };
                    println!("    {mark} {}", one_line(&content, 100));
                }
                ContentBlock::Text { .. } => {}
            }
        }
    }
    Ok(())
}
