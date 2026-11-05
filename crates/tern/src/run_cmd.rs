//! `tern run` and `tern resume`: non-interactive runs.

use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::bail;
use async_trait::async_trait;
use tern_core::TernHome;
use tern_core::agent::{RunOutcome, RunStatus};
use tern_core::approval::{
    ApprovalHandler, ApprovalOutcome, ApprovalRequest, AutoApprove, AutoDeny, Decision,
};
use tern_core::events::AgentEvent;
use tokio_util::sync::CancellationToken;

use crate::runtime::Runtime;

pub struct RunArgs<'a> {
    pub prompt: &'a str,
    pub session: Option<&'a str>,
    pub model: Option<&'a str>,
    pub yes: bool,
}

pub fn run(home: &TernHome, args: RunArgs<'_>) -> anyhow::Result<ExitCode> {
    let rt = Runtime::open(home, approvals(args.yes))?;
    let run = rt.start_run(args.session, args.model, args.prompt, "cli", "user")?;
    let banner = format!(
        "run {} (session {}, {})",
        run.run_id, run.session_id, run.model
    );
    drive(&rt, &run.run_id, false, &banner)
}

pub fn resume(home: &TernHome, run_id: &str, yes: bool) -> anyhow::Result<ExitCode> {
    let rt = Runtime::open(home, approvals(yes))?;
    let Some(run) = rt.store.run(run_id)? else {
        bail!("run {run_id} not found");
    };
    let banner = format!("resuming run {} ({})", run.id, run.status);
    drive(&rt, run_id, true, &banner)
}

/// Prints `banner` once Ctrl+C handling is in place, so anything that reacts
/// to it (a user, a test) can't interrupt before cancellation works.
fn drive(rt: &Runtime, run_id: &str, resume: bool, banner: &str) -> anyhow::Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let outcome: RunOutcome = runtime.block_on(async {
        let cancel = CancellationToken::new();
        let interrupts = Interrupts::register()?;
        eprintln!("{banner}");
        tokio::spawn(watch_interrupts(interrupts, cancel.clone()));
        let printer = tokio::spawn(print_progress(rt.agent.subscribe()));
        let result = if resume {
            rt.agent.resume(run_id, cancel).await
        } else {
            rt.agent.drive(run_id, cancel).await
        };
        printer.abort();
        anyhow::Ok(result?)
    })?;
    match outcome.status {
        RunStatus::Completed => {
            println!("{}", outcome.final_text.trim_end());
            Ok(ExitCode::SUCCESS)
        }
        RunStatus::Interrupted => {
            eprintln!(
                "interrupted; continue with `tern resume {}`",
                outcome.run_id
            );
            Ok(ExitCode::from(130))
        }
        RunStatus::Failed | RunStatus::Running => {
            let raw = outcome.error.as_deref().unwrap_or("unknown error");
            let model = rt
                .store
                .run(&outcome.run_id)
                .ok()
                .flatten()
                .and_then(|r| r.model)
                .unwrap_or_default();
            let text = crate::providers::explain_failure(&rt.live(), &model, raw);
            eprintln!(
                "run failed: {text}\ncontinue with `tern resume {}` (or fix the provider with `tern setup`)",
                outcome.run_id
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

/// SIGINT listener. On unix it is registered when created, not on first poll.
struct Interrupts {
    #[cfg(unix)]
    sig: tokio::signal::unix::Signal,
}

impl Interrupts {
    fn register() -> std::io::Result<Self> {
        Ok(Self {
            #[cfg(unix)]
            sig: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
        })
    }

    async fn recv(&mut self) -> bool {
        #[cfg(unix)]
        return self.sig.recv().await.is_some();
        #[cfg(not(unix))]
        return tokio::signal::ctrl_c().await.is_ok();
    }
}

/// First Ctrl+C cancels the run; a second one exits immediately.
async fn watch_interrupts(mut interrupts: Interrupts, cancel: CancellationToken) {
    if interrupts.recv().await {
        eprintln!("\ncancelling... (Ctrl+C again to quit)");
        cancel.cancel();
        if interrupts.recv().await {
            std::process::exit(130);
        }
    }
}

async fn print_progress(mut rx: tokio::sync::broadcast::Receiver<AgentEvent>) {
    use tokio::sync::broadcast::error::RecvError;
    let mut agents = Agents::default();
    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => return,
        };
        if let Some(line) = agents.line(&ev) {
            eprintln!("{line}");
            continue;
        }
        if agents.is_child(ev.run_id()) {
            continue;
        }
        match ev {
            AgentEvent::ToolStarted { summary, .. } => eprintln!("→ {summary}"),
            AgentEvent::ToolFinished {
                is_error,
                preview,
                replayed,
                ..
            } => {
                let mark = if is_error { "✗" } else { "✓" };
                let tag = if replayed { " (replayed)" } else { "" };
                eprintln!("  {mark} {preview}{tag}");
            }
            AgentEvent::Approval {
                name,
                outcome: ApprovalOutcome::Denied | ApprovalOutcome::Timeout,
                ..
            } => {
                eprintln!("  ✗ {name}: not approved");
            }
            AgentEvent::Retrying {
                attempt,
                delay_ms,
                error,
                ..
            } => {
                eprintln!(
                    "  model call failed ({error}); retry {attempt} in {:.1}s",
                    delay_ms as f64 / 1000.0
                );
            }
            _ => {}
        }
    }
}

/// Sub-agent progress, one line each: `[research-1] ✓ read_file src/main.rs`.
#[derive(Default)]
struct Agents {
    /// Child run id → label.
    labels: HashMap<String, String>,
    /// (run id, tool call id) → summary, for children's calls in progress.
    calls: HashMap<(String, String), String>,
}

impl Agents {
    fn is_child(&self, run_id: &str) -> bool {
        self.labels.contains_key(run_id)
    }

    /// The line to print for `ev`, if it is about a sub-agent.
    fn line(&mut self, ev: &AgentEvent) -> Option<String> {
        if let AgentEvent::SubagentSpawned {
            child_run_id,
            label,
            status,
            ..
        } = ev
        {
            self.labels.insert(child_run_id.clone(), label.clone());
            let what = if status == "queued" {
                "queued"
            } else {
                "started"
            };
            return Some(format!("[{label}] {what}"));
        }
        // The parent answering or steering a child.
        if let AgentEvent::SubagentMessage {
            to_run_id,
            kind,
            text,
            ..
        } = ev
            && let Some(label) = self.labels.get(to_run_id)
            && kind != "question"
        {
            return Some(format!("[{label}] ← {}", first_line(text)));
        }
        let label = self.labels.get(ev.run_id())?.clone();
        Some(match ev {
            AgentEvent::ToolStarted { id, summary, .. } => {
                self.calls
                    .insert((ev.run_id().to_owned(), id.clone()), summary.clone());
                return None;
            }
            AgentEvent::ToolFinished {
                id, is_error, name, ..
            } => {
                let summary = self
                    .calls
                    .remove(&(ev.run_id().to_owned(), id.clone()))
                    .unwrap_or_else(|| name.clone());
                let mark = if *is_error { "✗" } else { "✓" };
                format!("[{label}] {mark} {summary}")
            }
            AgentEvent::Approval {
                name,
                outcome: ApprovalOutcome::Denied | ApprovalOutcome::Timeout,
                ..
            } => format!("[{label}] ✗ {name}: not approved"),
            AgentEvent::SubagentMessage { kind, text, .. } if kind == "question" => {
                format!("[{label}] asks: {}", first_line(text))
            }
            AgentEvent::RunFinished { status, error, .. } => match error {
                Some(e) => format!("[{label}] {status}: {e}"),
                None => format!("[{label}] {status}"),
            },
            _ => return None,
        })
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect()
}

fn approvals(yes: bool) -> Arc<dyn ApprovalHandler> {
    if yes {
        Arc::new(AutoApprove)
    } else if std::io::stdin().is_terminal() && std::io::stderr().is_terminal() {
        Arc::new(TerminalApprover)
    } else {
        Arc::new(AutoDeny)
    }
}

/// Asks on the terminal: [y]es once, [a]lways, or [N]o.
struct TerminalApprover;

#[async_trait]
impl ApprovalHandler for TerminalApprover {
    async fn ask(&self, req: &ApprovalRequest) -> Decision {
        let always = match &req.pattern {
            Some(p) => format!(" / [a]lways ({} {p})", req.call.name),
            None => String::new(),
        };
        let who = req
            .agent_label
            .as_deref()
            .map(|l| format!("[{l}] "))
            .unwrap_or_default();
        let why = req
            .taint_note()
            .map(|n| format!("⚠ {n}\n"))
            .unwrap_or_default();
        let egress = match &req.egress_host {
            Some(h) => format!(" / all sites this [r]un / always [h]ost {h}"),
            None => String::new(),
        };
        let prompt = format!(
            "{why}{who}Allow {}? [y]es{always}{egress} / [N]o: ",
            req.headline()
        );
        tokio::task::spawn_blocking(move || {
            let mut stderr = std::io::stderr();
            let _ = write!(stderr, "{prompt}");
            let _ = stderr.flush();
            let mut line = String::new();
            if std::io::stdin().lock().read_line(&mut line).is_err() {
                return Decision::Deny;
            }
            match line.trim().to_ascii_lowercase().as_str() {
                "y" | "yes" => Decision::AllowOnce,
                "a" | "always" => Decision::AlwaysAllow,
                "r" | "run" => Decision::AllowRun,
                "h" | "host" => Decision::AlwaysAllowHost,
                _ => Decision::Deny,
            }
        })
        .await
        .unwrap_or(Decision::Deny)
    }
}
