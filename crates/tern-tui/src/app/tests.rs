use std::sync::Mutex;

use async_trait::async_trait;
use crossterm::event::{KeyEventState, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;
use tern_core::SecretStatus;
use tern_core::approval::ApprovalRequest;
use tern_core::subagent::ChildRun;
use tern_core::types::{ContentBlock, Role, ToolCall};
use tern_store::SessionSummary;
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::{HistoryMessage, Started};

/// (session, model, text) passed to `start_run`.
type StartCall = (Option<String>, Option<String>, String);

#[derive(Default)]
struct Fake {
    started: Mutex<Vec<StartCall>>,
    models_set: Mutex<Vec<(String, String)>>,
    reloads: Mutex<u32>,
    /// secrets.toml: name → value.
    secrets: Mutex<std::collections::BTreeMap<String, String>>,
    /// (run, text) passed to `message_run`.
    messaged: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl Backend for Fake {
    fn default_model(&self) -> Option<String> {
        Some("mock:default".into())
    }
    fn check_model(&self, model: &str) -> BackendResult<()> {
        if model.starts_with("mock:") {
            Ok(())
        } else {
            Err(format!("unknown provider in {model}"))
        }
    }
    fn start_run(
        &self,
        session: Option<&str>,
        model: Option<&str>,
        text: &str,
    ) -> BackendResult<Started> {
        let mut s = self.started.lock().unwrap();
        s.push((session.map(Into::into), model.map(Into::into), text.into()));
        Ok(Started {
            session_id: session.unwrap_or("s1").into(),
            run_id: format!("r{}", s.len()),
            model: model.unwrap_or("mock:default").into(),
        })
    }
    async fn drive(&self, _: &str, _: bool, _: CancellationToken) -> BackendResult<RunOutcome> {
        unreachable!("the app never drives runs itself")
    }
    fn set_session_model(&self, session: &str, model: &str) -> BackendResult<()> {
        self.models_set
            .lock()
            .unwrap()
            .push((session.into(), model.into()));
        Ok(())
    }
    fn sessions(&self, _: usize) -> BackendResult<Vec<SessionSummary>> {
        Ok(vec![])
    }
    fn session(&self, id: &str) -> BackendResult<Option<(SessionInfo, Vec<HistoryMessage>)>> {
        Ok((id == "old").then(|| {
            (
                SessionInfo {
                    id: "old".into(),
                    title: Some("earlier chat".into()),
                    model: Some("mock:old".into()),
                },
                vec![HistoryMessage {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "hello again".into(),
                    }],
                    summary: false,
                }],
            )
        }))
    }
    fn run_session(&self, run_id: &str) -> BackendResult<Option<String>> {
        Ok((run_id == "run-old").then(|| "old".into()))
    }
    fn tools(&self) -> Vec<(String, String)> {
        vec![("read_file".into(), "Read a file.\nMore.".into())]
    }
    fn usage(&self, _: &str) -> BackendResult<Usage> {
        Ok(Usage {
            input_tokens: 100,
            output_tokens: 20,
            ..Usage::default()
        })
    }
    fn reload(&self) -> BackendResult<String> {
        *self.reloads.lock().unwrap() += 1;
        Ok("reloaded".into())
    }
    fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        broadcast::channel(1).1
    }
    fn secrets(&self) -> BackendResult<Vec<SecretStatus>> {
        let file = self.secrets.lock().unwrap();
        let mut all: Vec<SecretStatus> = file
            .keys()
            .map(|name| SecretStatus {
                name: name.clone(),
                in_file: true,
                in_env: false,
                used_by: vec![],
            })
            .collect();
        if !file.contains_key("API_KEY") {
            all.insert(
                0,
                SecretStatus {
                    name: "API_KEY".into(),
                    in_file: false,
                    in_env: false,
                    used_by: vec!["provider mock".into()],
                },
            );
        }
        Ok(all)
    }
    fn set_secret(&self, name: &str, value: &str) -> BackendResult<()> {
        self.secrets
            .lock()
            .unwrap()
            .insert(name.into(), value.into());
        Ok(())
    }
    fn remove_secret(&self, name: &str) -> BackendResult<bool> {
        Ok(self.secrets.lock().unwrap().remove(name).is_some())
    }
    fn children(&self, run_id: &str) -> BackendResult<Vec<ChildRun>> {
        if run_id != "r1" {
            return Ok(vec![]);
        }
        Ok(vec![ChildRun {
            run_id: "c1".into(),
            session_id: "old".into(),
            parent_run_id: "r1".into(),
            call_id: "call_1".into(),
            label: "research".into(),
            task: "SUBTASK".into(),
            model: Some("mock:m".into()),
            depth: 1,
            status: "question".into(),
            error: None,
            started_at: 1,
            finished_at: None,
            usage: Usage::default(),
            result_preview: None,
            pending_question: Some("Private companies too?".into()),
        }])
    }
    fn message_run(&self, run_id: &str, text: &str) -> BackendResult<()> {
        self.messaged
            .lock()
            .unwrap()
            .push((run_id.into(), text.into()));
        Ok(())
    }
}

fn new_app() -> (App, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    (App::new(fake.clone()), fake)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn with(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        modifiers,
        ..key(code)
    }
}

fn type_text(app: &mut App, s: &str) {
    for c in s.chars() {
        app.on_key(key(KeyCode::Char(c)), Instant::now());
    }
}

fn submit(app: &mut App, s: &str) -> Vec<Effect> {
    type_text(app, s);
    app.on_key(key(KeyCode::Enter), Instant::now())
}

fn outcome(status: RunStatus) -> BackendResult<RunOutcome> {
    Ok(RunOutcome {
        run_id: "r1".into(),
        status,
        final_text: String::new(),
        error: None,
        usage: Usage::default(),
    })
}

fn notices(app: &App) -> String {
    app.entries
        .iter()
        .filter_map(|e| match e {
            Entry::Notice(s) | Entry::Error(s) => Some(s.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn screen(app: &mut App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| crate::ui::draw(f, app)).unwrap();
    let buf = t.backend().buffer().clone();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn enter_sends_and_starts_a_run() {
    let (mut app, fake) = new_app();
    let fx = submit(&mut app, "hello");
    assert_eq!(
        fx,
        [Effect::Drive {
            run_id: "r1".into(),
            resume: false
        }]
    );
    assert_eq!(app.entries, [Entry::User("hello".into())]);
    assert!(app.is_running());
    assert_eq!(app.session.as_ref().unwrap().id, "s1");
    assert!(app.input.text().is_empty());

    app.on_run_done(outcome(RunStatus::Completed));
    submit(&mut app, "again");
    let started = fake.started.lock().unwrap();
    assert_eq!(
        started[1].0.as_deref(),
        Some("s1"),
        "second message reuses the session"
    );
}

#[test]
fn shift_or_alt_enter_and_ctrl_j_insert_newlines() {
    let (mut app, fake) = new_app();
    type_text(&mut app, "a");
    app.on_key(with(KeyCode::Enter, KeyModifiers::SHIFT), Instant::now());
    type_text(&mut app, "b");
    app.on_key(with(KeyCode::Enter, KeyModifiers::ALT), Instant::now());
    app.on_key(
        with(KeyCode::Char('j'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    type_text(&mut app, "c");
    assert_eq!(app.input.text(), "a\nb\n\nc");
    assert!(fake.started.lock().unwrap().is_empty());
}

#[test]
fn streaming_text_tools_and_final_message() {
    let (mut app, _) = new_app();
    submit(&mut app, "go");
    let run_id = "r1".to_owned();
    app.on_agent_event(AgentEvent::TextDelta {
        run_id: run_id.clone(),
        text: "Let me ".into(),
    });
    app.on_agent_event(AgentEvent::TextDelta {
        run_id: run_id.clone(),
        text: "check".into(),
    });
    assert_eq!(
        app.entries[1],
        Entry::Assistant {
            text: "Let me check".into(),
            streaming: true
        }
    );
    app.on_agent_event(AgentEvent::AssistantMessage {
        run_id: run_id.clone(),
        text: "Let me check.".into(),
        replayed: false,
    });
    app.on_agent_event(AgentEvent::ToolStarted {
        run_id: run_id.clone(),
        id: "t1".into(),
        name: "shell".into(),
        summary: "shell ls".into(),
    });
    assert!(matches!(
        &app.entries[2],
        Entry::Tool {
            state: ToolState::Running,
            ..
        }
    ));
    app.on_agent_event(AgentEvent::ToolFinished {
        run_id: run_id.clone(),
        id: "t1".into(),
        name: "shell".into(),
        is_error: false,
        preview: "exit code 0".into(),
        output: "exit code 0\n--- stdout ---\na.txt".into(),
        replayed: false,
    });
    assert!(matches!(
        &app.entries[2],
        Entry::Tool {
            state: ToolState::Ok,
            ..
        }
    ));
    // A tool-only turn produces no empty assistant entry.
    app.on_agent_event(AgentEvent::AssistantMessage {
        run_id,
        text: String::new(),
        replayed: false,
    });
    assert_eq!(app.entries.len(), 3);

    let collapsed = screen(&mut app, 60, 12);
    assert!(
        collapsed.contains("✓ shell ls — exit code 0"),
        "{collapsed}"
    );
    assert!(!collapsed.contains("a.txt"));
    app.on_key(
        with(KeyCode::Char('o'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    let expanded = screen(&mut app, 60, 12);
    assert!(expanded.contains("│ a.txt"), "{expanded}");
}

#[test]
fn approvals_are_answered_inline() {
    let (mut app, _) = new_app();
    submit(&mut app, "go");
    let (tx, mut rx) = oneshot::channel();
    app.on_approval(PendingApproval {
        request: ApprovalRequest {
            id: "a1".into(),
            run_id: "r1".into(),
            session_id: "s1".into(),
            call: ToolCall {
                id: "t".into(),
                name: "shell".into(),
                input: json!({"command": "git push"}),
            },
            summary: "shell git push".into(),
            pattern: Some("git *".into()),
            parent_run_id: None,
            agent_label: None,
        },
        reply: tx,
    });
    let s = screen(&mut app, 70, 12);
    assert!(s.contains("Allow shell git push?"), "{s}");
    assert!(s.contains("[a] always (shell git *)"), "{s}");
    // Typing other keys does nothing while an approval is pending.
    type_text(&mut app, "x");
    assert!(app.input.text().is_empty());
    assert!(rx.try_recv().is_err());
    app.on_key(key(KeyCode::Char('a')), Instant::now());
    assert_eq!(rx.try_recv().unwrap(), Decision::AlwaysAllow);
    assert!(app.approval.is_none());
}

#[test]
fn ctrl_c_cancels_then_quits_within_a_second() {
    let (mut app, _) = new_app();
    submit(&mut app, "go");
    let t0 = Instant::now();
    let ctrl_c = with(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.on_key(ctrl_c, t0), [Effect::Cancel]);
    assert_eq!(
        app.on_key(ctrl_c, t0 + Duration::from_millis(500)),
        [Effect::Quit]
    );

    // Two presses more than a second apart don't quit.
    let (mut app, _) = new_app();
    type_text(&mut app, "draft");
    assert!(app.on_key(ctrl_c, t0).is_empty());
    assert!(app.input.text().is_empty(), "idle Ctrl+C clears the input");
    assert!(
        app.on_key(ctrl_c, t0 + Duration::from_millis(1500))
            .is_empty()
    );
}

#[test]
fn ctrl_c_denies_a_pending_approval() {
    let (mut app, _) = new_app();
    submit(&mut app, "go");
    let (tx, mut rx) = oneshot::channel();
    app.on_approval(PendingApproval {
        request: ApprovalRequest {
            id: "a1".into(),
            run_id: "r1".into(),
            session_id: "s1".into(),
            call: ToolCall {
                id: "t".into(),
                name: "shell".into(),
                input: json!({}),
            },
            summary: "shell".into(),
            pattern: Some("*".into()),
            parent_run_id: None,
            agent_label: None,
        },
        reply: tx,
    });
    let fx = app.on_key(
        with(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    assert_eq!(fx, [Effect::Cancel]);
    assert_eq!(rx.try_recv().unwrap(), Decision::Deny);
}

#[test]
fn new_message_while_running_cancels_then_sends() {
    let (mut app, fake) = new_app();
    submit(&mut app, "first");
    assert_eq!(submit(&mut app, "second"), [Effect::Cancel]);
    app.on_agent_event(AgentEvent::ToolStarted {
        run_id: "r1".into(),
        id: "t".into(),
        name: "x".into(),
        summary: "x".into(),
    });
    let fx = app.on_run_done(outcome(RunStatus::Interrupted));
    assert_eq!(
        fx,
        [Effect::Drive {
            run_id: "r2".into(),
            resume: false
        }]
    );
    assert_eq!(fake.started.lock().unwrap()[1].2, "second");
    assert!(app.entries.iter().any(|e| matches!(
        e,
        Entry::Tool {
            state: ToolState::Interrupted,
            ..
        }
    )));
    assert!(
        !notices(&app).contains("/resume r1"),
        "no resume hint when a queued message replaces the run"
    );
}

#[test]
fn interrupted_and_failed_runs_explain_how_to_resume() {
    let (mut app, _) = new_app();
    submit(&mut app, "go");
    app.on_run_done(outcome(RunStatus::Interrupted));
    assert!(notices(&app).contains("/resume r1"));
    submit(&mut app, "go");
    let mut failed = outcome(RunStatus::Failed).unwrap();
    failed.error = Some("HTTP 400".into());
    app.on_run_done(Ok(failed));
    assert!(
        notices(&app).contains("HTTP 400 — /resume r2 to retry"),
        "{}",
        notices(&app)
    );
}

#[test]
fn slash_commands() {
    let (mut app, fake) = new_app();
    submit(&mut app, "/help");
    assert!(notices(&app).contains("/resume <id>"));

    submit(&mut app, "/model openai:gpt");
    assert!(notices(&app).contains("unknown provider"));
    submit(&mut app, "/model mock:other");
    assert_eq!(app.model.as_deref(), Some("mock:other"));
    submit(&mut app, "hi");
    assert_eq!(
        fake.started.lock().unwrap()[0].1.as_deref(),
        Some("mock:other")
    );
    app.on_run_done(outcome(RunStatus::Completed));
    submit(&mut app, "/model mock:third");
    assert_eq!(
        fake.models_set.lock().unwrap()[0],
        ("s1".into(), "mock:third".into())
    );

    submit(&mut app, "/usage");
    assert!(notices(&app).contains("100 input, 20 output"));
    submit(&mut app, "/tools");
    assert!(notices(&app).contains("read_file — Read a file."));
    assert_eq!(submit(&mut app, "/reload"), [Effect::Resubscribe]);
    assert_eq!(*fake.reloads.lock().unwrap(), 1);
    submit(&mut app, "/stop");
    assert!(notices(&app).contains("nothing is running"));
    submit(&mut app, "/bogus");
    assert!(notices(&app).contains("unknown command /bogus"));

    submit(&mut app, "/new");
    assert!(app.session.is_none());
    assert_eq!(app.model.as_deref(), Some("mock:default"));
    assert_eq!(submit(&mut app, "/quit"), [Effect::Quit]);
}

#[test]
fn busy_commands_are_refused_while_running() {
    let (mut app, fake) = new_app();
    submit(&mut app, "go");
    for cmd in ["/new", "/reload", "/resume old"] {
        assert!(submit(&mut app, cmd).is_empty());
    }
    assert!(notices(&app).contains("/stop it first"));
    assert_eq!(*fake.reloads.lock().unwrap(), 0);
    assert_eq!(submit(&mut app, "/stop"), [Effect::Cancel]);
}

#[test]
fn resume_opens_sessions_and_resumes_runs() {
    let (mut app, _) = new_app();
    submit(&mut app, "/resume old");
    assert_eq!(app.session.as_ref().unwrap().id, "old");
    assert_eq!(app.model.as_deref(), Some("mock:old"));
    assert_eq!(app.entries[0], Entry::User("hello again".into()));
    assert!(!app.is_running());

    let (mut app, _) = new_app();
    let fx = submit(&mut app, "/resume run-old");
    assert_eq!(
        fx,
        [Effect::Drive {
            run_id: "run-old".into(),
            resume: true
        }]
    );
    assert!(app.is_running());

    let (mut app, _) = new_app();
    submit(&mut app, "/resume nope");
    assert!(notices(&app).contains("no session or run with id nope"));
}

#[test]
fn start_errors_keep_the_draft() {
    struct NoModel;
    #[async_trait]
    impl Backend for NoModel {
        fn default_model(&self) -> Option<String> {
            None
        }
        fn check_model(&self, _: &str) -> BackendResult<()> {
            Ok(())
        }
        fn start_run(&self, _: Option<&str>, _: Option<&str>, _: &str) -> BackendResult<Started> {
            Err("no model selected".into())
        }
        async fn drive(&self, _: &str, _: bool, _: CancellationToken) -> BackendResult<RunOutcome> {
            unreachable!()
        }
        fn set_session_model(&self, _: &str, _: &str) -> BackendResult<()> {
            Ok(())
        }
        fn sessions(&self, _: usize) -> BackendResult<Vec<SessionSummary>> {
            Ok(vec![])
        }
        fn session(&self, _: &str) -> BackendResult<Option<(SessionInfo, Vec<HistoryMessage>)>> {
            Ok(None)
        }
        fn run_session(&self, _: &str) -> BackendResult<Option<String>> {
            Ok(None)
        }
        fn tools(&self) -> Vec<(String, String)> {
            vec![]
        }
        fn usage(&self, _: &str) -> BackendResult<Usage> {
            Ok(Usage::default())
        }
        fn reload(&self) -> BackendResult<String> {
            Ok(String::new())
        }
        fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
            broadcast::channel(1).1
        }
    }
    let mut app = App::new(Arc::new(NoModel));
    assert!(submit(&mut app, "important draft").is_empty());
    assert_eq!(app.input.text(), "important draft");
    assert!(notices(&app).contains("no model selected"));
}

#[test]
fn transcript_scrolls_and_status_shows_state() {
    let (mut app, _) = new_app();
    for i in 0..30 {
        app.entries.push(Entry::Notice(format!("line {i}")));
    }
    let bottom = screen(&mut app, 50, 10);
    assert!(
        bottom.contains("line 29") && !bottom.contains("line 20 "),
        "{bottom}"
    );
    app.on_key(key(KeyCode::PageUp), Instant::now());
    let up = screen(&mut app, 50, 10);
    assert!(!up.contains("line 29"), "{up}");
    // Scrolling past the top clamps.
    for _ in 0..20 {
        app.on_key(key(KeyCode::PageUp), Instant::now());
    }
    assert!(screen(&mut app, 50, 10).contains("line 0"));
    assert!(
        bottom.contains("mock:default") && bottom.contains("ready"),
        "{bottom}"
    );

    submit(&mut app, "go");
    assert!(screen(&mut app, 60, 10).contains("running"));
}

#[test]
fn paste_keeps_newlines() {
    let (mut app, _) = new_app();
    app.on_paste("one\r\ntwo\rthree");
    assert_eq!(app.input.text(), "one\ntwo\nthree");
}

#[test]
fn secrets_panel_sets_values_without_showing_them() {
    let (mut app, fake) = new_app();
    submit(&mut app, "/secrets");
    let shown = screen(&mut app, 80, 20);
    assert!(
        shown.contains("API_KEY") && shown.contains("missing"),
        "{shown}"
    );
    assert!(shown.contains("provider mock"), "{shown}");

    // Enter on the selected (missing) secret asks for its value.
    app.on_key(key(KeyCode::Enter), Instant::now());
    type_text(&mut app, "sk-typed");
    app.on_paste("-pasted\n");
    let shown = screen(&mut app, 80, 20);
    assert!(shown.contains("New value for API_KEY"), "{shown}");
    assert!(shown.contains("15 characters"), "{shown}");
    assert!(
        !shown.contains("sk-typed") && !shown.contains("pasted"),
        "{shown}"
    );
    app.on_key(key(KeyCode::Enter), Instant::now());

    assert_eq!(
        fake.secrets
            .lock()
            .unwrap()
            .get("API_KEY")
            .map(String::as_str),
        Some("sk-typed-pasted")
    );
    assert!(
        app.input.text().is_empty(),
        "nothing leaks into the chat input"
    );
    assert!(fake.started.lock().unwrap().is_empty(), "nothing is sent");
    let shown = screen(&mut app, 80, 20);
    assert!(
        shown.contains("saved API_KEY") && shown.contains("set"),
        "{shown}"
    );

    // A new one by name, then remove it.
    app.on_key(key(KeyCode::Char('n')), Instant::now());
    type_text(&mut app, "github_token");
    app.on_key(key(KeyCode::Enter), Instant::now());
    type_text(&mut app, "ghp_x");
    app.on_key(key(KeyCode::Enter), Instant::now());
    assert!(fake.secrets.lock().unwrap().contains_key("GITHUB_TOKEN"));
    app.on_key(key(KeyCode::Char('d')), Instant::now());
    assert!(screen(&mut app, 80, 20).contains("Remove GITHUB_TOKEN"));
    app.on_key(key(KeyCode::Char('y')), Instant::now());
    assert!(!fake.secrets.lock().unwrap().contains_key("GITHUB_TOKEN"));

    app.on_key(key(KeyCode::Esc), Instant::now());
    assert!(app.secrets.is_none());
}

#[test]
fn secrets_panel_rejects_bad_names_and_closes_on_ctrl_c() {
    let (mut app, fake) = new_app();
    submit(&mut app, "/secrets 9bad");
    assert!(app.secrets.is_none());
    assert!(notices(&app).contains("invalid secret name"));

    submit(&mut app, "/secrets TOKEN");
    type_text(&mut app, "half-typed");
    app.on_key(
        with(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    assert!(app.secrets.is_none());
    assert!(fake.secrets.lock().unwrap().is_empty());
    assert!(app.input.text().is_empty());
}

fn spawned(parent: &str, child: &str, label: &str) -> AgentEvent {
    AgentEvent::SubagentSpawned {
        run_id: parent.into(),
        call_id: format!("call_{label}"),
        child_run_id: child.into(),
        child_session_id: format!("s_{child}"),
        label: label.into(),
        task: "SUBTASK".into(),
        model: "mock:m".into(),
        status: "running".into(),
    }
}

fn agent_lines(app: &App) -> Vec<AgentLine> {
    app.entries
        .iter()
        .filter_map(|e| match e {
            Entry::Agent(a) => Some(a.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn sub_agent_events_update_their_line_and_never_the_transcript() {
    let (mut app, _) = new_app();
    submit(&mut app, "fan out");
    app.on_agent_event(AgentEvent::ToolStarted {
        run_id: "r1".into(),
        id: "call_research".into(),
        name: "agent_spawn".into(),
        summary: "agent_spawn research".into(),
    });
    app.on_agent_event(spawned("r1", "c1", "research"));
    let before = app.entries.len();
    for ev in [
        AgentEvent::RunStarted {
            run_id: "c1".into(),
            session_id: "s_c1".into(),
            parent_run_id: Some("r1".into()),
        },
        AgentEvent::TextDelta {
            run_id: "c1".into(),
            text: "child text".into(),
        },
        AgentEvent::ToolStarted {
            run_id: "c1".into(),
            id: "t1".into(),
            name: "read_file".into(),
            summary: "read_file src/main.rs".into(),
        },
    ] {
        app.on_agent_event(ev);
    }
    let line = &agent_lines(&app)[0];
    assert_eq!(line.state, AgentState::Running);
    assert_eq!(line.activity, "Running read_file src/main.rs");
    assert!(
        screen(&mut app, 80, 12).contains("⑂ research  Running read_file src/main.rs · 1 tool")
    );

    for ev in [
        AgentEvent::ToolFinished {
            run_id: "c1".into(),
            id: "t1".into(),
            name: "read_file".into(),
            is_error: false,
            preview: "fn main".into(),
            output: "fn main".into(),
            replayed: false,
        },
        AgentEvent::Usage {
            run_id: "c1".into(),
            usage: Usage {
                input_tokens: 1500,
                output_tokens: 500,
                ..Usage::default()
            },
        },
        AgentEvent::AssistantMessage {
            run_id: "c1".into(),
            text: "The report.\nDetails.".into(),
            replayed: false,
        },
        AgentEvent::RunFinished {
            run_id: "c1".into(),
            status: "completed".into(),
            error: None,
        },
        // Another client's run: ignored.
        AgentEvent::TextDelta {
            run_id: "elsewhere".into(),
            text: "not ours".into(),
        },
    ] {
        app.on_agent_event(ev);
    }
    // No text or tool entries from the child, and none for agent_spawn.
    assert_eq!(app.entries.len(), before);
    assert!(
        !app.entries
            .iter()
            .any(|e| matches!(e, Entry::Assistant { .. } | Entry::Tool { .. })),
        "{:?}",
        app.entries
    );
    let line = &agent_lines(&app)[0];
    assert_eq!(line.state, AgentState::Completed);
    assert_eq!(line.tokens, 2000);
    assert_eq!(app.live_agents(), 0);
    let s = screen(&mut app, 80, 12);
    assert!(
        s.contains("✓ research  0:00 · 1 tool · 2k tok → The report."),
        "{s}"
    );

    // The parent's own text still lands in the transcript.
    app.on_agent_event(AgentEvent::TextDelta {
        run_id: "r1".into(),
        text: "parent text".into(),
    });
    assert!(
        matches!(app.entries.last(), Some(Entry::Assistant { text, .. }) if text == "parent text")
    );
}

#[test]
fn questions_and_answers_show_on_the_agent_line() {
    let (mut app, _) = new_app();
    submit(&mut app, "fan out");
    app.on_agent_event(spawned("r1", "c1", "scan"));
    app.on_agent_event(AgentEvent::SubagentMessage {
        run_id: "c1".into(),
        to_run_id: "r1".into(),
        kind: "question".into(),
        text: "Private too?".into(),
    });
    let line = &agent_lines(&app)[0];
    assert_eq!(line.state, AgentState::Question);
    assert_eq!(line.activity, "Asked: Private too?");
    assert_eq!(app.live_agents(), 1);
    assert!(screen(&mut app, 80, 12).contains("waiting on 1 agent"));
    app.on_agent_event(AgentEvent::SubagentMessage {
        run_id: "r1".into(),
        to_run_id: "c1".into(),
        kind: "answer".into(),
        text: "No.".into(),
    });
    let line = &agent_lines(&app)[0];
    assert_eq!(line.state, AgentState::Running);
    assert_eq!(line.activity, "Answered: No.");
}

fn approval(
    id: &str,
    run: &str,
    label: Option<&str>,
) -> (PendingApproval, oneshot::Receiver<Decision>) {
    let (tx, rx) = oneshot::channel();
    (
        PendingApproval {
            request: ApprovalRequest {
                id: id.into(),
                run_id: run.into(),
                session_id: "s".into(),
                call: ToolCall {
                    id: format!("t_{id}"),
                    name: "shell".into(),
                    input: json!({"command": "ls"}),
                },
                summary: format!("shell ls {id}"),
                pattern: None,
                parent_run_id: label.map(|_| "r1".into()),
                agent_label: label.map(Into::into),
            },
            reply: tx,
        },
        rx,
    )
}

#[test]
fn approvals_from_sub_agents_are_labelled_and_answered_in_turn() {
    let (mut app, _) = new_app();
    submit(&mut app, "fan out");
    app.on_agent_event(spawned("r1", "c1", "research"));
    app.on_agent_event(spawned("r1", "c2", "review"));
    let (a1, mut rx1) = approval("a1", "c1", Some("research"));
    let (a2, mut rx2) = approval("a2", "c2", Some("review"));
    app.on_approval(a1);
    app.on_approval(a2);
    assert_eq!(agent_lines(&app)[0].state, AgentState::Approval);
    let s = screen(&mut app, 80, 14);
    assert!(s.contains("[research] Allow shell ls a1?"), "{s}");
    assert!(s.contains("! research  Waiting for approval"), "{s}");

    app.on_key(key(KeyCode::Char('y')), Instant::now());
    assert_eq!(rx1.try_recv().unwrap(), Decision::AllowOnce);
    assert!(rx2.try_recv().is_err(), "the second waits its turn");
    let s = screen(&mut app, 80, 14);
    assert!(s.contains("[review] Allow shell ls a2?"), "{s}");

    // Stopping the run denies whatever is still waiting.
    app.on_key(
        with(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    assert_eq!(rx2.try_recv().unwrap(), Decision::Deny);
    assert!(app.approval.is_none());
}

#[test]
fn agents_commands_list_show_and_message_sub_agents() {
    let (mut app, fake) = new_app();
    submit(&mut app, "/agents");
    assert!(notices(&app).contains("no run yet"));
    submit(&mut app, "fan out");

    submit(&mut app, "/agents");
    let n = notices(&app);
    assert!(
        n.contains("research  question  c1  asked: Private companies too?"),
        "{n}"
    );

    submit(&mut app, "/agent nobody");
    assert!(notices(&app).contains("no sub-agent \"nobody\"; the last run's are: research"));

    submit(&mut app, "/agent research");
    let o = app.overlay.as_ref().expect("overlay open");
    assert!(o.title.contains("research"));
    let s = screen(&mut app, 60, 12);
    assert!(s.contains("hello again"), "{s}");
    // Keys go to the overlay, not the input; Esc closes it.
    type_text(&mut app, "x");
    assert!(app.input.text().is_empty());
    app.on_key(key(KeyCode::Esc), Instant::now());
    assert!(app.overlay.is_none());

    submit(&mut app, "/agent research public only, please");
    assert_eq!(
        fake.messaged.lock().unwrap().as_slice(),
        [("c1".to_owned(), "public only, please".to_owned())]
    );
    assert!(notices(&app).contains("sent to research"));
}
