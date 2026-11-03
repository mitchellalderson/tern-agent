//! `tern run` and `tern resume`: non-interactive runs.

use std::io::{BufRead, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
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
    let session = match args.session {
        Some(id) => rt
            .store
            .session(id)?
            .with_context(|| format!("session {id} not found"))?,
        None => rt
            .store
            .create_session(&tern_core::agent::new_id(), "cli", None)?,
    };
    let model = rt.pick_model(args.model, session.model.as_deref())?;
    if session.model.as_deref() != Some(&model.to_string()) {
        rt.store
            .set_session_model(&session.id, &model.to_string())?;
    }
    let system = tern_core::prompt::system_prompt(&tern_core::prompt::PromptInputs {
        home: &rt.home,
        now: chrono::Local::now(),
        workspace: &rt.config.workspace_dir(),
    });
    let run = rt
        .agent
        .start_run(&session.id, args.prompt, &model, &system, "user")?;
    let banner = format!("run {} (session {}, {model})", run.id, session.id);
    drive(&rt, &run.id, false, &banner)
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
            eprintln!(
                "run failed: {}\ncontinue with `tern resume {}`",
                outcome.error.as_deref().unwrap_or("unknown error"),
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
    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => return,
        };
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
        let prompt = format!(
            "Allow {}? [y]es / [a]lways ({} {}) / [N]o: ",
            req.summary, req.call.name, req.pattern
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
                _ => Decision::Deny,
            }
        })
        .await
        .unwrap_or(Decision::Deny)
    }
}
