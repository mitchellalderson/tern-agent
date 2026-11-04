use std::sync::Mutex;

use async_trait::async_trait;
use crossterm::event::{KeyEventState, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use serde_json::json;
use tern_core::SecretStatus;
use tern_core::approval::{ApprovalRequest, SecretAsk, SecretRequest, SecretValues};
use tern_core::config::McpServerConfig;
use tern_core::mcp::{
    ImportedServer, McpErrorKind, McpPatch, McpState, McpStatus, McpToolInfo, McpVerified,
    McpVerifyError,
};
use tern_core::subagent::ChildRun;
use tern_core::types::{ContentBlock, Role, ToolCall};
use tern_store::SessionSummary;
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::{HistoryMessage, PendingSecret, Started, UserPrompt};

/// (session, model, text) passed to `start_run`.
type StartCall = (Option<String>, Option<String>, String);

/// (id, approve, input, note) passed to `decide_outbox`.
type Decided = (i64, bool, Option<serde_json::Value>, Option<String>);

#[derive(Default)]
struct Fake {
    started: Mutex<Vec<StartCall>>,
    models_set: Mutex<Vec<(String, String)>>,
    reloads: Mutex<u32>,
    /// secrets.toml: name → value.
    secrets: Mutex<std::collections::BTreeMap<String, String>>,
    /// (run, text) passed to `message_run`.
    messaged: Mutex<Vec<(String, String)>>,
    mcp: Mutex<Vec<McpStatus>>,
    /// (name, patch) passed to `patch_mcp`.
    mcp_patches: Mutex<Vec<(String, McpPatch)>>,
    /// (name, remove_secrets) passed to `remove_mcp`.
    mcp_removed: Mutex<Vec<(String, bool)>>,
    /// (agent, text, new_topic) passed to `send_to_agent`.
    agent_sends: Mutex<Vec<(String, String, bool)>>,
    /// Pending outbox items, and (id, approve, input, note) decisions.
    outbox: Mutex<Vec<tern_core::outbox::OutboxItem>>,
    decided: Mutex<Vec<Decided>>,
    /// (channel, text) passed to `send_to_channel`; posts per channel;
    /// channels made; suggestions decided.
    channel_sends: Mutex<Vec<(String, String)>>,
    channel_posts: Mutex<std::collections::BTreeMap<String, Vec<crate::PostLine>>>,
    channels_made: Mutex<Vec<String>>,
    suggestions: Mutex<Vec<(i64, bool)>>,
    /// (what, id, slug, title) passed to `rename`; (what, id, choice)
    /// passed to `delete`.
    renames: Mutex<Vec<(crate::Managed, String, Option<String>, Option<String>)>>,
    deletes: Mutex<Vec<(crate::Managed, String, crate::DeleteChoice)>>,
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
    fn outbox(&self) -> BackendResult<Vec<tern_core::outbox::OutboxItem>> {
        Ok(self.outbox.lock().unwrap().clone())
    }
    fn decide_outbox(
        &self,
        id: i64,
        d: crate::outbox::Decide,
    ) -> BackendResult<tern_core::outbox::OutboxItem> {
        let mut items = self.outbox.lock().unwrap();
        let i = items
            .iter()
            .position(|o| o.id == id)
            .ok_or("no such item")?;
        let item = items.remove(i);
        self.decided
            .lock()
            .unwrap()
            .push((id, d.approve, d.input, d.note));
        Ok(item)
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
            agent_id: None,
        }])
    }
    fn mcp_servers(&self) -> BackendResult<Vec<McpStatus>> {
        Ok(self.mcp.lock().unwrap().clone())
    }
    fn import_mcp(&self, text: &str) -> BackendResult<Vec<ImportedServer>> {
        if !text.contains("mcpServers") {
            return Err("not valid JSON".into());
        }
        Ok(vec![ImportedServer {
            name: "github".into(),
            config: McpServerConfig {
                enabled: true,
                command: Some("github-mcp-server".into()),
                args: vec!["stdio".into()],
                env_secrets: vec!["GITHUB_TOKEN".into()],
                ..Default::default()
            },
            secrets: [("GITHUB_TOKEN".to_owned(), None)].into(),
            warnings: vec!["enter a value for GITHUB_TOKEN".into()],
        }])
    }
    fn patch_mcp(&self, name: &str, patch: &McpPatch) -> BackendResult<McpStatus> {
        self.mcp_patches
            .lock()
            .unwrap()
            .push((name.into(), patch.clone()));
        let mut all = self.mcp.lock().unwrap();
        let s = all.iter_mut().find(|s| s.name == name).ok_or("not found")?;
        if let Some(e) = patch.enabled {
            s.config.enabled = e;
            s.state = if e {
                McpState::NotStarted
            } else {
                McpState::Disabled
            };
        }
        Ok(s.clone())
    }
    fn remove_mcp(&self, name: &str, remove_secrets: bool) -> BackendResult<()> {
        self.mcp_removed
            .lock()
            .unwrap()
            .push((name.into(), remove_secrets));
        self.mcp.lock().unwrap().retain(|s| s.name != name);
        Ok(())
    }
    fn message_run(&self, run_id: &str, text: &str) -> BackendResult<()> {
        self.messaged
            .lock()
            .unwrap()
            .push((run_id.into(), text.into()));
        Ok(())
    }
    fn agents(&self) -> BackendResult<Vec<AgentInfo>> {
        let a = |id: &str, slug: &str, name: &str, needs_you| AgentInfo {
            id: id.into(),
            slug: slug.into(),
            name: name.into(),
            needs_you,
            active_runs: 0,
            is_default: id == "tern",
        };
        Ok(vec![
            a("tern", "tern", "tern", 0),
            a("tern:inbox", "inbox", "Inbox", 2),
            a("tern:research", "research", "Research", 0),
        ])
    }
    fn channels(&self) -> BackendResult<Vec<crate::ChannelInfo>> {
        let c = |id: &str, slug: &str, kind: &str, needs_you| crate::ChannelInfo {
            id: id.into(),
            slug: slug.into(),
            title: if slug == "home" {
                "Home".into()
            } else {
                slug.into()
            },
            kind: kind.into(),
            needs_you,
            active_runs: 0,
        };
        let mut l = vec![
            c("home", "home", "main", 3),
            c("dm:inbox", "dm-inbox", "main", 0),
            c("c-fin", "finances", "topic", 1),
        ];
        for s in self.channels_made.lock().unwrap().iter() {
            l.push(c(&format!("c-{s}"), s, "topic", 0));
        }
        Ok(l)
    }
    fn create_channel(&self, slug: &str, _: Option<&str>) -> BackendResult<crate::ChannelInfo> {
        self.channels_made.lock().unwrap().push(slug.into());
        Ok(crate::ChannelInfo {
            id: format!("c-{slug}"),
            slug: slug.into(),
            title: slug.into(),
            kind: "topic".into(),
            needs_you: 0,
            active_runs: 0,
        })
    }
    fn rename(
        &self,
        what: crate::Managed,
        id: &str,
        slug: Option<&str>,
        title: Option<&str>,
    ) -> BackendResult<String> {
        self.renames.lock().unwrap().push((
            what,
            id.into(),
            slug.map(Into::into),
            title.map(Into::into),
        ));
        Ok("renamed".into())
    }
    fn delete_preview(&self, what: crate::Managed, id: &str) -> BackendResult<String> {
        if id == "tern:research" {
            return Err("1 run(s) of @research are in progress".into());
        }
        Ok(format!("Delete {what:?} {id}?"))
    }
    fn delete(
        &self,
        what: crate::Managed,
        id: &str,
        choice: &crate::DeleteChoice,
    ) -> BackendResult<String> {
        self.deletes
            .lock()
            .unwrap()
            .push((what, id.into(), choice.clone()));
        Ok(format!("deleted {id}"))
    }
    fn send_to_channel(&self, id: &str, text: &str, _: Option<&str>) -> BackendResult<Started> {
        let mut s = self.channel_sends.lock().unwrap();
        s.push((id.into(), text.into()));
        Ok(Started {
            session_id: format!("{id}-main"),
            run_id: format!("rc{}", s.len()),
            model: "mock:default".into(),
        })
    }
    fn channel_posts(&self, id: &str, _: usize) -> BackendResult<Vec<crate::PostLine>> {
        Ok(self
            .channel_posts
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default())
    }
    fn decide_suggestion(&self, id: i64, accept: bool) -> BackendResult<String> {
        self.suggestions.lock().unwrap().push((id, accept));
        if accept {
            self.channels_made.lock().unwrap().push("travel".into());
        }
        Ok(if accept {
            "created #travel".into()
        } else {
            "declined".into()
        })
    }
    fn send_to_agent(
        &self,
        id: &str,
        text: &str,
        _: Option<&str>,
        new_topic: bool,
    ) -> BackendResult<Started> {
        let mut s = self.agent_sends.lock().unwrap();
        s.push((id.into(), text.into(), new_topic));
        Ok(Started {
            session_id: format!("dm-{id}-{}", s.len()),
            run_id: format!("ra{}", s.len()),
            model: "mock:default".into(),
        })
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
        quiet: false,
        tool_result: None,
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
            effect: tern_core::tool::Effect::Destructive,
            pattern: Some("git *".into()),
            parent_run_id: None,
            agent_label: None,
            taint: None,
            local_only: false,
            egress_host: None,
        },
        reply: tx,
    });
    let s = screen(&mut app, 70, 12);
    assert!(s.contains("Allow destructive · shell git push?"), "{s}");
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
            effect: tern_core::tool::Effect::Write,
            pattern: Some("*".into()),
            parent_run_id: None,
            agent_label: None,
            taint: None,
            local_only: false,
            egress_host: None,
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

fn outbox_item(id: i64, to: &str) -> tern_core::outbox::OutboxItem {
    serde_json::from_value(serde_json::json!({
        "id": id, "run_id": "r", "session_id": "s", "tool": "gmail_send",
        "input": { "to": to }, "summary": format!("gmail_send to {to}"), "effect": "write",
        "taint": "result of inbox", "state": "pending", "created_at": 0, "expires_at": 0
    }))
    .unwrap()
}

#[test]
fn outbox_panel_approves_edits_and_rejects() {
    let (mut app, fake) = new_app();
    *fake.outbox.lock().unwrap() = vec![
        outbox_item(12, "bob@example.com"),
        outbox_item(13, "dana@example.com"),
        outbox_item(14, "eve@example.com"),
    ];
    submit(&mut app, "/outbox");
    let shown = screen(&mut app, 100, 24);
    assert!(
        shown.contains("#12   write · gmail_send to bob@example.com"),
        "{shown}"
    );
    assert!(
        shown.contains("this run read untrusted content (result of inbox)"),
        "{shown}"
    );

    // Approve the first as it is.
    app.on_key(key(KeyCode::Char('a')), Instant::now());
    assert!(screen(&mut app, 100, 24).contains("#12 approved"));
    // Edit the next one's arguments, then approve.
    app.on_key(key(KeyCode::Char('e')), Instant::now());
    for _ in 0..40 {
        app.on_key(key(KeyCode::Backspace), Instant::now());
    }
    type_text(&mut app, r#"{"to": "carol@example.com"}"#);
    app.on_key(key(KeyCode::Enter), Instant::now());
    // Reject the last with a note.
    app.on_key(key(KeyCode::Char('r')), Instant::now());
    type_text(&mut app, "not to Eve");
    app.on_key(key(KeyCode::Enter), Instant::now());

    let decided = fake.decided.lock().unwrap().clone();
    assert_eq!(decided[0], (12, true, None, None));
    assert_eq!(
        decided[1],
        (
            13,
            true,
            Some(serde_json::json!({ "to": "carol@example.com" })),
            None
        )
    );
    assert_eq!(decided[2], (14, false, None, Some("not to Eve".into())));
    assert!(screen(&mut app, 100, 24).contains("Nothing is waiting for you."));
    assert!(app.input.text().is_empty() && fake.started.lock().unwrap().is_empty());
    app.on_key(key(KeyCode::Esc), Instant::now());
    assert!(app.outbox.is_none());
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
        agent: None,
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
            quiet: false,
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
                effect: tern_core::tool::Effect::Write,
                pattern: None,
                parent_run_id: label.map(|_| "r1".into()),
                agent_label: label.map(Into::into),
                taint: None,
                local_only: false,
                egress_host: None,
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
    assert!(s.contains("[research] Allow write · shell ls a1?"), "{s}");
    assert!(s.contains("! research  Waiting for approval"), "{s}");

    app.on_key(key(KeyCode::Char('y')), Instant::now());
    assert_eq!(rx1.try_recv().unwrap(), Decision::AllowOnce);
    assert!(rx2.try_recv().is_err(), "the second waits its turn");
    let s = screen(&mut app, 80, 14);
    assert!(s.contains("[review] Allow write · shell ls a2?"), "{s}");

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

fn secret_request(call: &str, names: &[&str]) -> (PendingSecret, oneshot::Receiver<SecretValues>) {
    let (reply, rx) = oneshot::channel();
    let asks = names
        .iter()
        .map(|n| SecretAsk {
            name: (*n).into(),
            in_file: false,
            in_env: false,
            used_by: vec!["connector weather (draft) · reaches api.weather.com".into()],
        })
        .collect();
    let request = SecretRequest {
        id: format!("q-{call}"),
        run_id: "r1".into(),
        session_id: "s1".into(),
        call_id: Some(call.into()),
        asks,
        reason: "the weather connector sends it as a bearer token".into(),
        replace: false,
        parent_run_id: None,
        agent_label: None,
    };
    (PendingSecret { request, reply }, rx)
}

#[test]
fn secret_prompts_take_values_without_showing_them() {
    let (mut app, _) = new_app();
    submit(&mut app, "set up weather");
    let (p, mut rx) = secret_request("c1", &["WEATHER_KEY", "WEATHER_ID"]);
    app.on_prompt(UserPrompt::Secret(p));

    let s = screen(&mut app, 90, 24);
    assert!(s.contains("The agent is asking for secrets"), "{s}");
    assert!(s.contains("The agent says: \"the weather connector"), "{s}");
    assert!(s.contains("used by connector weather (draft)"), "{s}");
    assert!(s.contains("secret needed"), "{s}");

    // Enter starts typing into the prompt. A paste with a trailing newline
    // doesn't submit; Tab moves on.
    app.on_key(key(KeyCode::Enter), Instant::now());
    app.on_paste("wk-0123456789abcdefXYZ\n");
    assert!(rx.try_recv().is_err());
    let s = screen(&mut app, 90, 24);
    assert!(!s.contains("0123456789abcdef"), "value shown: {s}");
    assert!(s.contains("wk-012…fXYZ"), "{s}");
    app.on_key(key(KeyCode::Tab), Instant::now());
    type_text(&mut app, "id-42");
    // Keys went to the prompt, not the chat input.
    assert!(app.input.text().is_empty());
    app.on_key(key(KeyCode::Enter), Instant::now());

    let values = rx.try_recv().unwrap();
    assert_eq!(values["WEATHER_KEY"], "wk-0123456789abcdefXYZ");
    assert_eq!(values["WEATHER_ID"], "id-42");
    assert!(app.secret_prompt.is_none());
    // Nothing of the value is left in the transcript or input history.
    let s = screen(&mut app, 90, 24);
    assert!(!s.contains("wk-0"), "{s}");
}

#[test]
fn secret_prompts_skip_queue_and_close_when_answered_elsewhere() {
    let (mut app, _) = new_app();
    submit(&mut app, "go");
    let (a, mut rx_a) = secret_request("c1", &["A_KEY"]);
    let (b, mut rx_b) = secret_request("c2", &["B_KEY"]);
    let (c, mut rx_c) = secret_request("c3", &["C_KEY"]);
    app.on_prompt(UserPrompt::Secret(a));
    app.on_prompt(UserPrompt::Secret(b));
    app.on_prompt(UserPrompt::Secret(c));

    // Esc skips: an empty answer.
    app.on_key(key(KeyCode::Esc), Instant::now());
    assert!(rx_a.try_recv().unwrap().is_empty());

    // B is answered by another client: its tool call finishes, the prompt
    // closes without answering, and C comes up.
    app.on_agent_event(AgentEvent::ToolFinished {
        run_id: "r1".into(),
        id: "c2".into(),
        name: "secret_request".into(),
        is_error: false,
        preview: String::new(),
        output: r#"{"B_KEY":"set"}"#.into(),
        replayed: false,
    });
    assert!(matches!(
        rx_b.try_recv(),
        Err(oneshot::error::TryRecvError::Closed)
    ));
    assert_eq!(app.secret_prompt.as_ref().unwrap().call_id(), Some("c3"));

    // Ctrl+C while running skips what's left along with cancelling.
    let effects = app.on_key(
        with(KeyCode::Char('c'), KeyModifiers::CONTROL),
        Instant::now(),
    );
    assert_eq!(effects, [Effect::Cancel]);
    assert!(rx_c.try_recv().unwrap().is_empty());
    assert!(app.secret_prompt.is_none());
}

#[test]
fn a_prompt_that_opens_mid_message_ignores_the_rest_of_it() {
    let (mut app, _) = new_app();
    submit(&mut app, "go");
    type_text(&mut app, "and also check tomorr");
    let (p, mut rx) = secret_request("c1", &["WEATHER_KEY"]);
    app.on_prompt(UserPrompt::Secret(p));
    // The rest of the message, and the Enter meant to send it.
    type_text(&mut app, "ow");
    app.on_paste("pasted into the chat");
    app.on_key(key(KeyCode::Enter), Instant::now());
    assert!(rx.try_recv().is_err(), "nothing saved");
    let prompt = app.secret_prompt.as_ref().unwrap();
    assert_eq!(prompt.values, [""]);
    assert!(prompt.armed);
}

#[test]
fn a_finished_run_skips_only_its_own_prompts() {
    let (mut app, _) = new_app();
    submit(&mut app, "go");
    let (mine, mut rx_mine) = secret_request("c1", &["MINE"]);
    let (mut other, mut rx_other) = secret_request("c2", &["OTHER"]);
    other.request.run_id = "someone-elses-run".into();
    app.on_prompt(UserPrompt::Secret(other));
    app.on_prompt(UserPrompt::Secret(mine));
    // The same request again (e.g. from the pending list) is ignored.
    let (dup, _) = secret_request("c2", &["OTHER"]);
    app.on_prompt(UserPrompt::Secret(dup));

    app.on_run_done(outcome(RunStatus::Completed));
    assert!(rx_mine.try_recv().unwrap().is_empty(), "own prompt skipped");
    assert!(rx_other.try_recv().is_err(), "another client's stays open");
    assert_eq!(app.secret_prompt.as_ref().unwrap().call_id(), Some("c2"));
}

fn mcp_server(name: &str, state: McpState) -> McpStatus {
    McpStatus {
        name: name.into(),
        config: McpServerConfig {
            enabled: state != McpState::Disabled,
            command: Some("fixture".into()),
            ..Default::default()
        },
        state,
        tools: vec![tool("echo"), tool("delete_all")],
        missing_secrets: vec![],
        server_info: Some("fixture 1.0".into()),
        stderr_tail: vec!["fixture server starting".into()],
    }
}

fn tool(name: &str) -> McpToolInfo {
    McpToolInfo {
        exposed: format!("mcp__fx__{name}"),
        remote: name.into(),
        description: format!("{name} things"),
        disabled: false,
    }
}

fn press(app: &mut App, code: KeyCode) -> Vec<Effect> {
    app.on_key(key(code), Instant::now())
}

#[test]
fn mcp_add_tests_then_saves_with_secrets_kept_out_of_view() {
    let (mut app, _fake) = new_app();
    submit(&mut app, "/mcp add");
    assert!(app.mcp.is_some());
    type_text(&mut app, "fx");
    press(&mut app, KeyCode::Down);
    type_text(&mut app, "python3");
    press(&mut app, KeyCode::Down);
    type_text(&mut app, "server.py --mode 'a b'");
    press(&mut app, KeyCode::Down);
    type_text(
        &mut app,
        "LOG_LEVEL=warn GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz",
    );
    // The token moves to secrets and is asked for (prefilled) before testing.
    assert!(press(&mut app, KeyCode::Enter).is_empty());
    let screen_text = screen(&mut app, 100, 30);
    assert!(screen_text.contains("GITHUB_TOKEN"), "{screen_text}");
    assert!(!screen_text.contains("ghp_abc"), "{screen_text}");
    let effects = press(&mut app, KeyCode::Enter);
    let Some(Effect::Task(Task::McpVerify { name, cfg, secrets })) = effects.first() else {
        panic!("no verify task: {effects:?}");
    };
    assert_eq!(name, "fx");
    assert_eq!(cfg.args, ["server.py", "--mode", "a b"]);
    assert_eq!(cfg.env_secrets, ["GITHUB_TOKEN"]);
    assert_eq!(cfg.env.keys().collect::<Vec<_>>(), ["LOG_LEVEL"]);
    assert_eq!(secrets["GITHUB_TOKEN"], "ghp_abcdefghijklmnopqrstuvwxyz");
    assert!(app.is_busy());

    app.on_task_done(TaskDone::McpVerified(Ok(Ok(McpVerified {
        tools: vec![tool("echo"), tool("delete_all")],
        server_info: Some("fixture 1.0".into()),
    }))));
    let screen_text = screen(&mut app, 100, 30);
    assert!(
        screen_text.contains("Connected to fixture 1.0"),
        "{screen_text}"
    );
    // Hide the second tool, then save.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char(' '));
    let effects = press(&mut app, KeyCode::Enter);
    let Some(Effect::Task(Task::McpSave {
        old, cfg, secrets, ..
    })) = effects.first()
    else {
        panic!("no save task: {effects:?}");
    };
    assert_eq!(*old, None);
    assert_eq!(cfg.disabled_tools, ["delete_all"]);
    assert!(secrets.contains_key("GITHUB_TOKEN"));

    app.on_task_done(TaskDone::McpSaved(Ok(mcp_server(
        "fx",
        McpState::Connecting,
    ))));
    let panel = app.mcp.as_ref().unwrap();
    assert_eq!(panel.mode, crate::mcp::Mode::List);
    assert!(panel.message.as_ref().unwrap().1.contains("saved fx"));
    // Nothing secret reached the chat.
    assert!(!notices(&app).contains("ghp_"));
    assert!(app.input.text().is_empty());
}

#[test]
fn mcp_failed_test_shows_stderr_and_can_save_anyway() {
    let (mut app, _fake) = new_app();
    submit(&mut app, "/mcp add");
    type_text(&mut app, "fx");
    press(&mut app, KeyCode::Down);
    type_text(&mut app, "missing-server");
    let effects = press(&mut app, KeyCode::Enter);
    assert!(matches!(
        effects.first(),
        Some(Effect::Task(Task::McpVerify { .. }))
    ));
    app.on_task_done(TaskDone::McpVerified(Ok(Err(McpVerifyError {
        kind: McpErrorKind::ExitedEarly,
        message: "connection closed (Error: bad token)".into(),
        stderr_tail: vec!["Error: bad token".into()],
    }))));
    let screen_text = screen(&mut app, 100, 30);
    assert!(
        screen_text.contains("exited while starting"),
        "{screen_text}"
    );
    assert!(screen_text.contains("Error: bad token"), "{screen_text}");
    let effects = press(&mut app, KeyCode::Char('s'));
    assert!(matches!(
        effects.first(),
        Some(Effect::Task(Task::McpSave { .. }))
    ));
}

#[test]
fn mcp_list_toggles_removes_and_restarts() {
    let (mut app, fake) = new_app();
    *fake.mcp.lock().unwrap() = vec![
        mcp_server("fx", McpState::Running { tools: 2, since: 0 }),
        mcp_server(
            "pg",
            McpState::Unavailable {
                error: "`pg-mcp` exited".into(),
                retry_at: None,
                failures: 2,
            },
        ),
    ];
    submit(&mut app, "/mcp");
    let screen_text = screen(&mut app, 100, 30);
    assert!(screen_text.contains("running · 2 tools"), "{screen_text}");
    assert!(
        screen_text.contains("unavailable · `pg-mcp` exited"),
        "{screen_text}"
    );

    // Space disables the selected server.
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(
        fake.mcp_patches.lock().unwrap()[0],
        (
            "fx".into(),
            McpPatch {
                enabled: Some(false),
                ..Default::default()
            }
        )
    );
    // Details: hide a tool, show logs.
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(
        fake.mcp_patches.lock().unwrap()[1].1.disabled_tools,
        Some(vec!["delete_all".to_owned()])
    );
    press(&mut app, KeyCode::Char('l'));
    assert!(screen(&mut app, 100, 30).contains("fixture server starting"));
    press(&mut app, KeyCode::Esc);

    // Restart a running server: a background task.
    press(&mut app, KeyCode::Down);
    fake.mcp.lock().unwrap()[1].config.enabled = true;
    let effects = press(&mut app, KeyCode::Char('r'));
    assert!(
        matches!(effects.first(), Some(Effect::Task(Task::McpRestart(n))) if n == "pg"),
        "{effects:?}"
    );

    // Remove with its secrets.
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(*fake.mcp_removed.lock().unwrap(), [("pg".into(), true)]);
    assert_eq!(app.mcp.as_ref().unwrap().items.len(), 1);
    press(&mut app, KeyCode::Esc);
    assert!(app.mcp.is_none());
}

#[test]
fn mcp_paste_opens_the_form_prefilled() {
    let (mut app, _fake) = new_app();
    submit(&mut app, "/mcp paste");
    app.on_paste("{\"mcpServers\": {\"github\": {\"command\": \"github-mcp-server\"}}}\n");
    press(&mut app, KeyCode::Enter);
    let Some(crate::mcp::Mode::Form(form)) = app.mcp.as_ref().map(|p| p.mode.clone()) else {
        panic!("no form");
    };
    assert_eq!(form.name, "github");
    assert_eq!(form.command, "github-mcp-server");
    assert_eq!(form.env_secrets, "GITHUB_TOKEN");
    // The missing secret is asked for next.
    press(&mut app, KeyCode::Enter);
    let screen_text = screen(&mut app, 100, 30);
    assert!(
        screen_text.contains("type or paste the value"),
        "{screen_text}"
    );
    // A pasted value lands in the secret field, not the chat input.
    app.on_paste("ghp_pasted_value_123456");
    assert!(app.input.text().is_empty());
    assert!(!screen(&mut app, 100, 30).contains("ghp_pasted"));
}

#[test]
fn mcp_shortcuts() {
    let (mut app, fake) = new_app();
    *fake.mcp.lock().unwrap() = vec![mcp_server("fx", McpState::NotStarted)];
    let effects = submit(&mut app, "/mcp restart fx");
    assert!(matches!(effects.first(), Some(Effect::Task(Task::McpRestart(n))) if n == "fx"));
    app.on_task_done(TaskDone::McpRestarted(
        "fx".into(),
        Ok(mcp_server("fx", McpState::Running { tools: 2, since: 0 })),
    ));
    assert!(notices(&app).contains("MCP server fx: running · 2 tools"));
    submit(&mut app, "/mcp disable fx");
    assert!(notices(&app).contains("MCP server fx disabled"));
    submit(&mut app, "/mcp restart");
    assert!(notices(&app).contains("usage: /mcp restart <name>"));
    submit(&mut app, "/tools");
    assert!(notices(&app).contains("MCP servers (/mcp to manage)"));
}

#[test]
fn agent_switches_to_a_profiles_dm_and_back() {
    let (mut app, fake) = new_app();
    submit(&mut app, "/agents");
    assert!(
        notices(&app).contains("@inbox  Inbox  (2 need you)"),
        "{}",
        notices(&app)
    );
    submit(&mut app, "/agent inbox");
    assert_eq!(
        app.profile.as_ref().map(|p| p.id.as_str()),
        Some("tern:inbox")
    );
    let fx = submit(&mut app, "anything urgent?");
    assert!(matches!(&fx[..], [Effect::Drive { run_id, .. }] if run_id == "ra1"));
    app.running = None;
    assert_eq!(
        fake.agent_sends.lock().unwrap()[0],
        ("tern:inbox".into(), "anything urgent?".into(), false)
    );
    assert!(fake.started.lock().unwrap().is_empty());
    // /new starts a new topic in the DM.
    submit(&mut app, "/new");
    submit(&mut app, "next thing");
    app.running = None;
    assert!(fake.agent_sends.lock().unwrap()[1].2);
    assert!(
        screen(&mut app, 80, 12).contains(" Inbox · "),
        "the status line names the agent"
    );
    // Back to tern: its own sessions again.
    submit(&mut app, "/agent tern");
    assert!(app.profile.is_none());
    submit(&mut app, "hello");
    assert_eq!(fake.started.lock().unwrap().len(), 1);
}

#[test]
fn a_sub_agent_label_wins_over_an_agent_name() {
    let (mut app, fake) = new_app();
    submit(&mut app, "start");
    app.running = None;
    // The last run (r1) has a sub-agent labelled "research".
    submit(&mut app, "/agent research keep going");
    assert!(app.profile.is_none());
    assert_eq!(fake.messaged.lock().unwrap().len(), 1);
    // With /agent <name> <message>, a profile gets the message in its DM.
    submit(&mut app, "/agent inbox what came in?");
    assert_eq!(app.profile.as_ref().map(|p| p.slug.as_str()), Some("inbox"));
    assert_eq!(fake.agent_sends.lock().unwrap()[0].1, "what came in?");
}

fn auth_card(
    id: &str,
    call: &str,
    card: tern_core::approval::AuthCard,
) -> (crate::PendingAuth, oneshot::Receiver<String>) {
    let (reply, rx) = oneshot::channel();
    let request = tern_core::approval::AuthPrompt {
        id: id.into(),
        run_id: "r1".into(),
        session_id: "s1".into(),
        call_id: Some(call.into()),
        parent_run_id: None,
        agent_label: None,
        card,
    };
    (crate::PendingAuth { request, reply }, rx)
}

#[test]
fn sign_in_cards_choose_open_and_close() {
    use tern_core::approval::{AuthCard, AuthOption};
    let (mut app, _) = new_app();
    submit(&mut app, "connect notion");
    let opt = |id: &str, rec: bool| AuthOption {
        id: id.into(),
        label: format!("{id} label"),
        detail: format!("{id} detail"),
        recommended: rec,
    };
    let (p, mut rx) = auth_card(
        "a1",
        "c1",
        AuthCard::Choose {
            title: "Connect Notion".into(),
            options: vec![opt("oauth", false), opt("key", true)],
            notes: vec!["Found via auth.notion.com metadata (verified).".into()],
        },
    );
    app.on_prompt(UserPrompt::Auth(p));
    let s = screen(&mut app, 90, 24);
    assert!(
        s.contains("Connect Notion") && s.contains("key label (recommended)"),
        "{s}"
    );
    assert!(
        s.contains("metadata (verified)") && s.contains("sign-in needed"),
        "{s}"
    );
    // The recommended option starts highlighted; ↑ then Enter picks the first.
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    assert_eq!(rx.try_recv().unwrap(), "oauth");
    assert!(app.auth_prompt.is_none());

    let (p, mut rx) = auth_card(
        "a2",
        "c2",
        AuthCard::SignIn {
            title: "Sign in to gmail · work".into(),
            url: Some("https://accounts.google.com/o/oauth2/v2/auth?x=1".into()),
            user_code: None,
            verification_uri: None,
            device_available: true,
            notes: vec![],
        },
    );
    app.on_prompt(UserPrompt::Auth(p));
    assert_eq!(
        press(&mut app, KeyCode::Enter),
        vec![Effect::OpenUrl(
            "https://accounts.google.com/o/oauth2/v2/auth?x=1".into()
        )]
    );
    assert!(rx.try_recv().is_err(), "opening doesn't answer");
    // A device code for the same call replaces it.
    let (p, _rx3) = auth_card(
        "a3",
        "c2",
        AuthCard::SignIn {
            title: "Sign in to gmail · work".into(),
            url: None,
            user_code: Some("WDJB-MJHT".into()),
            verification_uri: Some("https://www.google.com/device".into()),
            device_available: false,
            notes: vec![],
        },
    );
    app.on_prompt(UserPrompt::Auth(p));
    assert_eq!(
        app.auth_prompt.as_ref().map(|p| p.request.id.as_str()),
        Some("a3")
    );
    let s = screen(&mut app, 90, 24);
    assert!(s.contains("WDJB-MJHT"), "{s}");
    // The tool finishing closes it.
    app.on_agent_event(AgentEvent::ToolFinished {
        run_id: "r1".into(),
        id: "c2".into(),
        name: "connection_add".into(),
        is_error: false,
        preview: String::new(),
        output: String::new(),
        replayed: false,
    });
    assert!(app.auth_prompt.is_none());
    drop(rx);
}

fn post(id: i64, author: &str, kind: &str, text: &str, run: Option<&str>) -> crate::PostLine {
    crate::PostLine {
        id,
        author: author.into(),
        author_kind: if author == "you" { "user" } else { "agent" }.into(),
        kind: kind.into(),
        text: text.into(),
        run_id: run.map(Into::into),
        needs_you: kind == "needs_you",
        suggestion: None,
    }
}

#[test]
fn channel_commands_switch_post_and_show_what_needs_you() {
    let (mut app, fake) = new_app();
    fake.channel_posts.lock().unwrap().insert(
        "c-fin".into(),
        vec![
            post(1, "you", "message", "How much on cards?", None),
            post(2, "tern", "answer", "About 1,240 EUR.", Some("r0")),
        ],
    );
    submit(&mut app, "/channels");
    let n = notices(&app);
    assert!(n.contains("▸ Home  (3 need you)"), "{n}");
    assert!(n.contains("  #finances  (1 need you)"), "{n}");
    assert!(!n.contains("dm-inbox"), "DMs are /agent's");
    submit(&mut app, "/channel nope");
    assert!(notices(&app).contains("no channel #nope"));

    // Switching shows the recent posts; the status line says where.
    submit(&mut app, "/channel #finances");
    assert_eq!(app.channel.as_ref().unwrap().slug, "finances");
    let n = notices(&app);
    assert!(n.contains("tern: About 1,240 EUR."), "{n}");
    let s = screen(&mut app, 100, 12);
    assert!(s.contains("#finances · tern"), "{s}");
    assert!(s.contains("3 need you"), "{s}");

    // A send posts to the channel; afterwards, what others said is shown,
    // not the driven run's own answer (it streamed).
    let fx = submit(&mut app, "And Northwind?");
    assert!(matches!(fx.as_slice(), [Effect::Drive { run_id, .. }] if run_id == "rc1"));
    assert_eq!(
        fake.channel_sends.lock().unwrap().as_slice(),
        [("c-fin".to_owned(), "And Northwind?".to_owned())]
    );
    fake.channel_posts
        .lock()
        .unwrap()
        .get_mut("c-fin")
        .unwrap()
        .extend([
            post(3, "you", "message", "And Northwind?", None),
            post(4, "tern", "answer", "310 EUR.", Some("rc1")),
            post(
                5,
                "Research",
                "answer",
                "Northwind's terms are net 30.",
                Some("rk"),
            ),
        ]);
    app.on_run_done(outcome(RunStatus::Completed));
    let n = notices(&app);
    assert!(n.contains("Research: Northwind's terms are net 30."), "{n}");
    assert!(!n.contains("tern: 310 EUR."), "{n}");

    // A suggestion card in Home is answered by slug.
    fake.channel_posts.lock().unwrap().insert(
        "home".into(),
        vec![crate::PostLine {
            suggestion: Some((9, "travel".into(), true)),
            ..post(
                6,
                "tern",
                "needs_you",
                "Suggests a new channel, #travel: Trips.",
                None,
            )
        }],
    );
    submit(&mut app, "/channel accept travel");
    assert_eq!(fake.suggestions.lock().unwrap().as_slice(), [(9, true)]);
    assert_eq!(app.channel.as_ref().unwrap().slug, "travel");

    // A new channel, then Home again.
    submit(&mut app, "/channel new launch Q4 launch");
    assert_eq!(fake.channels_made.lock().unwrap().last().unwrap(), "launch");
    assert_eq!(app.channel.as_ref().unwrap().slug, "launch");
    submit(&mut app, "/channel home");
    assert!(app.channel.is_none());
    submit(&mut app, "hi");
    assert_eq!(
        fake.started.lock().unwrap().len(),
        1,
        "Home is the TUI's own sessions"
    );
    // /agent leaves the channel.
    submit(&mut app, "/channel finances");
    submit(&mut app, "/agent inbox");
    assert!(app.channel.is_none());
}

#[test]
fn channels_and_agents_are_renamed_and_deleted_only_after_confirming() {
    let (mut app, fake) = new_app();
    submit(&mut app, "/channel #finances");
    assert!(app.channel.is_some());

    // Rename and retitle go straight through.
    submit(&mut app, "/channel rename finances money");
    submit(&mut app, "/channel title #finances Money talk");
    assert_eq!(
        fake.renames.lock().unwrap().as_slice(),
        [
            (
                crate::Managed::Channel,
                "c-fin".to_owned(),
                Some("money".to_owned()),
                None
            ),
            (
                crate::Managed::Channel,
                "c-fin".to_owned(),
                None,
                Some("Money talk".to_owned())
            ),
        ]
    );

    // Deleting shows what it touches first, and does nothing until `yes`.
    submit(&mut app, "/channel delete finances");
    let n = notices(&app);
    assert!(n.contains("Delete Channel c-fin?"), "{n}");
    assert!(n.contains("Confirm with: /channel delete finances yes"), "{n}");
    assert!(fake.deletes.lock().unwrap().is_empty());
    submit(&mut app, "/channel delete finances yes");
    assert_eq!(fake.deletes.lock().unwrap().len(), 1);
    assert!(app.channel.is_none(), "the open channel was deleted: back in Home");
    // Home and DMs are refused before asking the backend.
    submit(&mut app, "/channel delete home yes");
    assert!(notices(&app).contains("isn't a topic channel"));
    assert_eq!(fake.deletes.lock().unwrap().len(), 1);

    // Agents: rename, slug, and a delete that says where routines go.
    submit(&mut app, "/agents rename inbox Mail Room");
    submit(&mut app, "/agents slug @inbox mail");
    let r = fake.renames.lock().unwrap();
    assert_eq!(
        r[2..],
        [
            (
                crate::Managed::Agent,
                "tern:inbox".to_owned(),
                None,
                Some("Mail Room".to_owned())
            ),
            (
                crate::Managed::Agent,
                "tern:inbox".to_owned(),
                Some("mail".to_owned()),
                None
            ),
        ]
    );
    drop(r);
    submit(&mut app, "/agents delete inbox to research");
    let n = notices(&app);
    assert!(
        n.contains("Confirm with: /agents delete inbox to research yes"),
        "{n}"
    );
    assert_eq!(fake.deletes.lock().unwrap().len(), 1);
    submit(&mut app, "/agents delete inbox to research yes");
    let d = fake.deletes.lock().unwrap();
    assert_eq!(
        d[1],
        (
            crate::Managed::Agent,
            "tern:inbox".to_owned(),
            crate::DeleteChoice {
                reassign_to: Some("research".into()),
                delete_routines: false
            }
        )
    );
    drop(d);
    // A refusal (work in progress) is shown, not confirmed.
    submit(&mut app, "/agents delete research");
    assert!(notices(&app).contains("in progress"));
    submit(&mut app, "/agents delete inbox routines yes");
    assert!(fake.deletes.lock().unwrap()[2].2.delete_routines);
}
