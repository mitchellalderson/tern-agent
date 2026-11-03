//! Terminal setup and the async event loop.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use futures::StreamExt;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::app::{App, Effect};
use crate::{Backend, PendingApproval, ui};

/// Run the TUI until the user quits. Restores the terminal on exit and on
/// panic. `command` is a slash command to run first (`/setup`, `/resume <id>`).
pub async fn run(
    backend: Arc<dyn Backend>,
    mut approvals: mpsc::Receiver<PendingApproval>,
    command: Option<String>,
) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, EnableBracketedPaste);
    // Lets terminals that support it report Shift+Enter distinctly.
    let enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }

    let result = event_loop(&mut terminal, backend, &mut approvals, command).await;

    if enhanced {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    backend: Arc<dyn Backend>,
    approvals: &mut mpsc::Receiver<PendingApproval>,
    command: Option<String>,
) -> io::Result<()> {
    let mut app = App::new(backend.clone());
    let mut agent_rx = backend.subscribe();
    let mut keys = EventStream::new();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel();
    let (task_tx, mut task_rx) = mpsc::unbounded_channel();
    let mut cancel: Option<CancellationToken> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(100));

    let mut effects = match command {
        // First-run setup may already be open; don't stack another.
        Some(c) if c == "/setup" && app.setup.is_some() => vec![],
        Some(c) => app.on_key_command(&c),
        None => vec![],
    };

    loop {
        for effect in effects.drain(..) {
            match effect {
                Effect::Quit => {
                    if let Some(c) = cancel.take() {
                        c.cancel();
                        // Give the run a moment to record itself as interrupted.
                        let _ = tokio::time::timeout(Duration::from_secs(3), done_rx.recv()).await;
                    }
                    return Ok(());
                }
                Effect::Cancel => {
                    if let Some(c) = &cancel {
                        c.cancel();
                    }
                }
                Effect::Resubscribe => agent_rx = backend.subscribe(),
                Effect::Task(task) => {
                    let (backend, tx) = (backend.clone(), task_tx.clone());
                    tokio::spawn(async move {
                        let _ = tx.send(task.run(backend.as_ref()).await);
                    });
                }
                Effect::Drive { run_id, resume } => {
                    let token = CancellationToken::new();
                    cancel = Some(token.clone());
                    let (backend, tx) = (backend.clone(), done_tx.clone());
                    tokio::spawn(async move {
                        let r = backend.drive(&run_id, resume, token).await;
                        let _ = tx.send(r);
                    });
                }
            }
        }

        terminal.draw(|f| ui::draw(f, &mut app))?;

        tokio::select! {
            ev = keys.next() => match ev {
                Some(Ok(Event::Key(k))) => effects = app.on_key(k, Instant::now()),
                Some(Ok(Event::Paste(s))) => app.on_paste(&s),
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e),
                None => return Ok(()),
            },
            ev = agent_rx.recv() => match ev {
                Ok(ev) => app.on_agent_event(ev),
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => agent_rx = backend.subscribe(),
            },
            Some(p) = approvals.recv() => app.on_approval(p),
            Some(result) = done_rx.recv() => {
                // Apply events the run emitted before it finished.
                while let Ok(ev) = agent_rx.try_recv() {
                    app.on_agent_event(ev);
                }
                cancel = None;
                effects = app.on_run_done(result);
            }
            Some(done) = task_rx.recv() => effects = app.on_task_done(done),
            _ = tick.tick(), if app.is_running() || app.is_busy() => {}
        }
    }
}
