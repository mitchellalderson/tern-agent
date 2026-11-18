use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use futures::executor::block_on;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tern_core::agent::RunOutcome;
use tern_core::events::AgentEvent;
use tern_core::models::ModelInfo;
use tern_core::types::Usage;
use tern_store::SessionSummary;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::app::{App, Effect};
use crate::{HistoryMessage, SessionInfo, Started};

#[derive(Default)]
struct Fake {
    providers: Mutex<Vec<ProviderStatus>>,
    defaults: Mutex<Vec<String>>,
    removed: Mutex<Vec<(String, bool)>>,
    verified: Mutex<Vec<ProviderDraft>>,
    needs_setup: bool,
    /// A background agent that manages Ollama, when set.
    local: Option<Mutex<LocalFake>>,
}

#[derive(Default)]
struct LocalFake {
    info: tern_core::models::LocalInfo,
    pulls: Vec<tern_core::models::LocalPullInfo>,
    models: Vec<tern_core::models::LocalModelInfo>,
    calls: Vec<String>,
    start_fails: bool,
}

fn models() -> Vec<ModelInfo> {
    ["m1", "m2"]
        .iter()
        .map(|id| ModelInfo {
            id: (*id).into(),
            context_window: Some(200_000),
            ..ModelInfo::default()
        })
        .collect()
}

fn status(name: &str, kind: ProviderKind) -> ProviderStatus {
    ProviderStatus {
        name: name.into(),
        kind,
        base_url: None,
        secret: Some("X_API_KEY".into()),
        key_source: "file".into(),
        key_hint: Some("sk-ant…good".into()),
        preset: None,
        label: name.into(),
        error: None,
        message: None,
        is_default: false,
    }
}

fn name_of(d: &ProviderDraft) -> String {
    d.name
        .clone()
        .or_else(|| d.preset.clone())
        .unwrap_or_else(|| match d.kind {
            Some(ProviderKind::Anthropic) => "anthropic".into(),
            _ => "local".into(),
        })
}

#[async_trait]
impl Backend for Fake {
    fn default_model(&self) -> Option<String> {
        self.defaults.lock().unwrap().last().cloned()
    }
    fn check_model(&self, _: &str) -> BackendResult<()> {
        Ok(())
    }
    fn resolve_model(&self, model: &str) -> BackendResult<Resolved> {
        let model = if model.contains(':') {
            model.to_owned()
        } else {
            format!("anthropic:{model}")
        };
        Ok(Resolved {
            model,
            warning: None,
        })
    }
    fn setup_needed(&self) -> bool {
        self.needs_setup
    }
    fn start_run(&self, _: Option<&str>, _: Option<&str>, _: &str) -> BackendResult<Started> {
        unreachable!()
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
    fn providers(&self) -> BackendResult<Vec<ProviderStatus>> {
        Ok(self.providers.lock().unwrap().clone())
    }
    async fn models(
        &self,
        provider: Option<String>,
        _: bool,
    ) -> BackendResult<Vec<ProviderModels>> {
        Ok(vec![ProviderModels {
            provider: provider.unwrap_or_else(|| "anthropic".into()),
            error: None,
            message: None,
            fetched_at: Some(0),
            models: models(),
            warnings: vec![],
            suggested: vec![],
            label: String::new(),
        }])
    }
    async fn verify_provider(
        &self,
        draft: ProviderDraft,
        _: Option<String>,
    ) -> BackendResult<Verified> {
        self.verified.lock().unwrap().push(draft.clone());
        let ok = draft.use_env_key || draft.key.as_deref() != Some("sk-bad");
        Ok(Verified {
            ok,
            name: name_of(&draft),
            error: None,
            message: (!ok).then(|| "anthropic rejected that key.".into()),
            warnings: vec![],
            suggested_base_url: None,
            key_hint: draft.key.as_deref().map(mask_key),
            models: if ok { models() } else { vec![] },
        })
    }
    async fn save_provider(
        &self,
        draft: ProviderDraft,
        _: Option<String>,
        default_model: Option<String>,
    ) -> BackendResult<ProviderStatus> {
        let mut s = status(
            &name_of(&draft),
            draft.kind.unwrap_or(ProviderKind::OpenaiCompat),
        );
        s.is_default = default_model.is_some();
        if let Some(m) = default_model {
            self.defaults.lock().unwrap().push(m);
        }
        self.providers.lock().unwrap().push(s.clone());
        Ok(s)
    }
    fn remove_provider(&self, name: &str, remove_secret: bool) -> BackendResult<()> {
        self.removed
            .lock()
            .unwrap()
            .push((name.into(), remove_secret));
        self.providers.lock().unwrap().retain(|p| p.name != name);
        Ok(())
    }
    fn local_first_run(&self) -> bool {
        self.local.is_some()
    }
    async fn local_status(&self) -> BackendResult<tern_core::models::LocalInfo> {
        Ok(self.local.as_ref().unwrap().lock().unwrap().info.clone())
    }
    async fn local_install(&self) -> BackendResult<()> {
        let mut l = self.local.as_ref().unwrap().lock().unwrap();
        l.calls.push("install".into());
        l.info.installed = true;
        Ok(())
    }
    async fn local_start(&self) -> BackendResult<()> {
        let mut l = self.local.as_ref().unwrap().lock().unwrap();
        l.calls.push("start".into());
        if l.start_fails {
            return Err("Another program is using port 11434.".into());
        }
        l.info.running = true;
        Ok(())
    }
    async fn local_pull(&self, model: &str) -> BackendResult<tern_core::models::LocalPullInfo> {
        let mut l = self.local.as_ref().unwrap().lock().unwrap();
        l.calls.push(format!("pull {model}"));
        let p = tern_core::models::LocalPullInfo {
            id: "p1".into(),
            model: model.into(),
            phase: "downloading".into(),
            ..Default::default()
        };
        l.pulls.push(p.clone());
        Ok(p)
    }
    async fn local_pulls(&self) -> BackendResult<Vec<tern_core::models::LocalPullInfo>> {
        Ok(self.local.as_ref().unwrap().lock().unwrap().pulls.clone())
    }
    async fn local_models(&self) -> BackendResult<Vec<tern_core::models::LocalModelInfo>> {
        Ok(self.local.as_ref().unwrap().lock().unwrap().models.clone())
    }
    fn set_default_model(&self, model: &str) -> BackendResult<Resolved> {
        self.defaults.lock().unwrap().push(model.into());
        Ok(Resolved {
            model: model.into(),
            warning: None,
        })
    }
}

fn press(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn typed(panel: &mut SetupPanel, backend: &Fake, text: &str) {
    for c in text.chars() {
        panel.on_key(press(KeyCode::Char(c)), backend);
    }
}

/// Run the task a key asked for and hand its result back to the panel.
fn finish(panel: &mut SetupPanel, backend: &Fake, action: SetupAction) {
    let SetupAction::Run(task) = action else {
        panic!("expected a task, got {action:?}");
    };
    let done = block_on(task.run(backend));
    panel.on_task_done(done, backend);
}

#[test]
fn adds_anthropic_after_a_rejected_key() {
    let b = Fake::default();
    let mut p = SetupPanel::open(&b).unwrap();
    // No providers yet: straight to choosing a kind.
    assert!(matches!(p.step, Step::Kind { .. }));
    p.on_key(press(KeyCode::Enter), &b);
    assert!(matches!(&p.step, Step::Key(f) if f.kind == ProviderKind::Anthropic));

    assert_eq!(p.on_key(press(KeyCode::Enter), &b), SetupAction::None);
    assert!(p.message.as_ref().unwrap().1.contains("Paste your API key"));

    p.on_paste("  sk-bad\n");
    let action = p.on_key(press(KeyCode::Enter), &b);
    assert!(matches!(p.step, Step::Verifying(_)));
    assert!(p.is_busy());
    // Keys do nothing while the check runs.
    assert_eq!(p.on_key(press(KeyCode::Esc), &b), SetupAction::None);
    finish(&mut p, &b, action);
    assert!(matches!(p.step, Step::Key(_)));
    assert!(p.message.as_ref().unwrap().1.contains("rejected"));

    p.on_key(ctrl('u'), &b);
    p.on_paste("sk-good");
    let action = p.on_key(press(KeyCode::Enter), &b);
    finish(&mut p, &b, action);
    let Step::Model {
        list, make_default, ..
    } = &p.step
    else {
        panic!("expected the model step, got {:?}", p.step);
    };
    assert_eq!(list.chosen().as_deref(), Some("anthropic:m1"));
    assert!(*make_default, "first provider becomes the default");

    p.on_key(press(KeyCode::Down), &b);
    let action = p.on_key(press(KeyCode::Enter), &b);
    assert_eq!(
        action,
        SetupAction::Run(Task::Save {
            draft: ProviderDraft {
                kind: Some(ProviderKind::Anthropic),
                name: Some("anthropic".into()),
                base_url: None,
                key: Some("sk-good".into()),
                use_env_key: false,
                preset: Some("anthropic".into()),
            },
            existing: None,
            default_model: Some("anthropic:m2".into()),
        })
    );
    finish(&mut p, &b, action);
    assert!(matches!(p.step, Step::List));
    assert!(p.changed);
    assert_eq!(p.providers.len(), 1);
    assert!(p.message.as_ref().unwrap().1.starts_with("Saved anthropic"));
}

#[test]
fn adds_an_openai_compatible_server_without_a_key() {
    let b = Fake::default();
    let mut p = SetupPanel::open(&b).unwrap();
    // "Other (OpenAI-compatible URL)" is the last choice.
    p.on_key(press(KeyCode::Char('9')), &b);
    assert!(matches!(p.step, Step::Endpoint { .. }));
    p.on_key(press(KeyCode::Enter), &b);
    assert!(p.message.as_ref().unwrap().1.contains("base URL"));

    typed(&mut p, &b, "http://localhost:11434/v1");
    p.on_key(press(KeyCode::Tab), &b);
    typed(&mut p, &b, "Ollama");
    p.on_key(press(KeyCode::Enter), &b);
    assert!(matches!(p.step, Step::Key(_)));
    // A blank key is fine for a local server.
    let action = p.on_key(press(KeyCode::Enter), &b);
    finish(&mut p, &b, action);
    assert!(matches!(p.step, Step::Model { .. }));
    let draft = b.verified.lock().unwrap()[0].clone();
    assert_eq!(draft.base_url.as_deref(), Some("http://localhost:11434/v1"));
    assert_eq!(draft.name.as_deref(), Some("ollama"));
    assert_eq!(draft.key, None);

    // A typed model id that isn't listed, without making it the default.
    typed(&mut p, &b, "llama3:8b");
    p.on_key(press(KeyCode::Tab), &b);
    let action = p.on_key(press(KeyCode::Enter), &b);
    let SetupAction::Run(Task::Save { default_model, .. }) = &action else {
        panic!("{action:?}");
    };
    assert_eq!(default_model, &None);
}

#[test]
fn uses_the_environment_key() {
    let b = Fake::default();
    let mut p = SetupPanel::open(&b).unwrap();
    p.on_key(press(KeyCode::Enter), &b);
    let action = p.on_key(ctrl('e'), &b);
    finish(&mut p, &b, action);
    assert!(matches!(p.step, Step::Model { .. }));
    let draft = b.verified.lock().unwrap()[0].clone();
    assert!(draft.use_env_key);
    assert_eq!(draft.key, None);
}

#[test]
fn manages_an_existing_provider() {
    let b = Fake::default();
    b.providers
        .lock()
        .unwrap()
        .push(status("anthropic", ProviderKind::Anthropic));
    let mut p = SetupPanel::open(&b).unwrap();
    assert!(matches!(p.step, Step::List));

    // Choose a default model, typing one the list doesn't have.
    p.on_key(press(KeyCode::Enter), &b);
    p.on_key(press(KeyCode::Down), &b);
    let action = p.on_key(press(KeyCode::Enter), &b);
    assert_eq!(
        action,
        SetupAction::Run(Task::Models {
            provider: Some("anthropic".into()),
            refresh: false
        })
    );
    finish(&mut p, &b, action);
    typed(&mut p, &b, "custom-model");
    p.on_key(press(KeyCode::Enter), &b);
    assert!(matches!(p.step, Step::List));
    assert_eq!(b.defaults.lock().unwrap()[..], ["anthropic:custom-model"]);

    // Change the key: it is checked against the existing provider.
    p.on_key(press(KeyCode::Enter), &b);
    p.on_key(press(KeyCode::Enter), &b);
    assert!(matches!(&p.step, Step::Key(f) if f.existing.as_deref() == Some("anthropic")));
    p.on_key(press(KeyCode::Esc), &b);

    // Remove it but keep its key.
    p.on_key(press(KeyCode::Char('d')), &b);
    p.on_key(press(KeyCode::Char('k')), &b);
    assert_eq!(b.removed.lock().unwrap()[..], [("anthropic".into(), false)]);
    assert!(p.providers.is_empty());
}

#[test]
fn first_run_opens_setup_and_hides_the_key() {
    let b = Arc::new(Fake {
        needs_setup: true,
        ..Fake::default()
    });
    let mut app = App::new(b.clone());
    let panel = app.setup.as_ref().expect("setup opens on first run");
    assert!(panel.intro.as_ref().unwrap().contains("Welcome"));

    app.on_key(press(KeyCode::Enter), std::time::Instant::now());
    app.on_paste("sk-ant-api03-very-secret-value-a1B2");
    let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
    term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let screen: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(screen.contains("sk-ant…a1B2"), "masked key shown");
    assert!(
        !screen.contains("very-secret"),
        "key leaked onto the screen"
    );
    assert!(screen.contains("console.anthropic.com"));

    // Saving a provider updates the model for the next run.
    let effects = app.on_key(press(KeyCode::Enter), std::time::Instant::now());
    let [Effect::Task(task)] = &effects[..] else {
        panic!("{effects:?}");
    };
    app.on_task_done(block_on(task.clone().run(b.as_ref())));
    let effects = app.on_key(press(KeyCode::Enter), std::time::Instant::now());
    let [Effect::Task(task)] = &effects[..] else {
        panic!("{effects:?}");
    };
    app.on_task_done(block_on(task.clone().run(b.as_ref())));
    assert_eq!(app.model.as_deref(), Some("anthropic:m1"));
}

#[test]
fn model_picker_lists_and_sets() {
    let b = Arc::new(Fake::default());
    let mut app = App::new(b.clone());
    assert!(app.setup.is_none());
    let effects = app.on_key_command("/model");
    let [Effect::Task(task)] = &effects[..] else {
        panic!("{effects:?}");
    };
    assert!(app.is_busy());
    app.on_task_done(block_on(task.clone().run(b.as_ref())));
    assert!(!app.is_busy());
    let now = std::time::Instant::now();
    app.on_key(press(KeyCode::Down), now);
    app.on_key(ctrl('d'), now);
    assert!(app.picker.is_none());
    assert_eq!(app.model.as_deref(), Some("anthropic:m2"));
    assert_eq!(b.defaults.lock().unwrap()[..], ["anthropic:m2"]);

    // Without the provider, resolved by the backend.
    app.on_key_command("/model m1");
    assert_eq!(app.model.as_deref(), Some("anthropic:m1"));
}

#[test]
fn a_preset_asks_only_for_the_key() {
    let b = Fake::default();
    let mut p = SetupPanel::open(&b).unwrap();
    // OpenRouter is the third choice.
    p.on_key(press(KeyCode::Char('3')), &b);
    let Step::Key(form) = &p.step else {
        panic!("expected the key step, got {:?}", p.step);
    };
    assert_eq!(form.base_url, "https://openrouter.ai/api/v1");
    assert_eq!(form.name, "openrouter");
    assert_eq!(form.label(), "OpenRouter");
    // OpenRouter needs a key, unlike a local server.
    assert_eq!(p.on_key(press(KeyCode::Enter), &b), SetupAction::None);
    assert!(p.message.as_ref().unwrap().1.contains("Paste your API key"));

    p.on_paste("sk-or-test");
    let action = p.on_key(press(KeyCode::Enter), &b);
    finish(&mut p, &b, action);
    assert!(matches!(p.step, Step::Model { .. }));
    let draft = b.verified.lock().unwrap()[0].clone();
    assert_eq!(draft.preset.as_deref(), Some("openrouter"));
    assert_eq!(
        draft.base_url.as_deref(),
        Some("https://openrouter.ai/api/v1")
    );
    assert_eq!(draft.name.as_deref(), Some("openrouter"));

    // Choosing a model never needs `openrouter:` typed by hand.
    let Step::Model { list, .. } = &p.step else {
        unreachable!()
    };
    assert_eq!(list.chosen().as_deref(), Some("openrouter:m1"));
}

#[test]
fn a_local_preset_needs_no_key_and_a_second_one_gets_a_new_name() {
    let b = Fake::default();
    b.providers
        .lock()
        .unwrap()
        .push(status("ollama", ProviderKind::OpenaiCompat));
    let mut p = SetupPanel::open(&b).unwrap();
    p.on_key(press(KeyCode::Char('a')), &b);
    // Ollama is the seventh choice.
    p.on_key(press(KeyCode::Char('7')), &b);
    let Step::Key(form) = &p.step else {
        panic!("{:?}", p.step);
    };
    assert_eq!(form.name, "ollama-2");
    assert!(form.key_optional());
    let action = p.on_key(press(KeyCode::Enter), &b);
    assert!(matches!(action, SetupAction::Run(Task::Verify { .. })));
}

#[test]
fn suggestions_fill_in_when_the_list_is_empty() {
    use crate::models::{ModelList, Row};
    let mut l = ModelList::new(None);
    l.set_groups(vec![ProviderModels {
        provider: "openrouter".into(),
        error: None,
        message: None,
        fetched_at: None,
        models: vec![],
        warnings: vec![],
        suggested: vec!["openai/gpt-4o-mini".into()],
        label: String::new(),
    }]);
    let rows = l.rows();
    assert!(matches!(&rows[0], Row::Header { note: Some(n), .. } if n.contains("suggestions")));
    assert_eq!(l.chosen().as_deref(), Some("openrouter:openai/gpt-4o-mini"));
}

// --- First run with Ollama ------------------------------------------------

fn local_fake(installed: bool, running: bool) -> Fake {
    let local = LocalFake {
        info: tern_core::models::LocalInfo {
            installed,
            running,
            install_page: "https://ollama.com/download".into(),
            install: tern_core::models::LocalPlanInfo {
                summary: "Downloads Ollama from its official GitHub release.".into(),
                available: true,
            },
            starter: Some(tern_core::models::LocalStarterInfo {
                model: "qwen3.5:9b".into(),
                size_gb: 6.6,
                blurb: "Good all-round.".into(),
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    Fake {
        needs_setup: true,
        local: Some(Mutex::new(local)),
        ..Fake::default()
    }
}

/// Run a poll task (with no wait) and hand its result to the panel.
fn run_poll(panel: &mut SetupPanel, fake: &Fake, task: Task) {
    let Task::LocalPoll(mut opts) = task else {
        panic!("expected a poll, got {task:?}");
    };
    opts.first = true;
    let view = block_on(crate::local_flow::poll(fake, opts));
    panel.on_task_done(TaskDone::Local(view), fake);
}

fn calls(f: &Fake) -> Vec<String> {
    f.local.as_ref().unwrap().lock().unwrap().calls.clone()
}

fn poll_opts(t: Option<Task>) -> crate::local_flow::PollOpts {
    match t {
        Some(Task::LocalPoll(o)) => o,
        other => panic!("expected a poll, got {other:?}"),
    }
}

#[test]
fn first_run_leads_with_ollama_and_sets_up_the_starter_as_the_default() {
    let fake = local_fake(false, false);
    let mut panel = SetupPanel::open(&fake).unwrap();
    assert!(matches!(panel.step, Step::Ollama(_)));
    // Nothing is installed or downloaded before the person agrees.
    let first = panel.first_task().unwrap();
    run_poll(&mut panel, &fake, first);
    let o = poll_opts(panel.take_task());
    assert!(!o.go && !o.install);
    assert!(calls(&fake).is_empty());

    panel.on_key(press(KeyCode::Enter), &fake);
    // The poll already in flight finishes with the old options; the next one
    // carries the agreement.
    run_poll(&mut panel, &fake, Task::LocalPoll(o));
    let o = poll_opts(panel.take_task());
    assert!(o.go && o.install);
    run_poll(&mut panel, &fake, Task::LocalPoll(o));
    // Installed, started by itself (no prompt), starter downloading.
    assert_eq!(calls(&fake), ["install", "start", "pull qwen3.5:9b"]);
    let o = poll_opts(panel.take_task());
    assert!(!o.install, "the install runs once");

    // The download finishes.
    {
        let mut l = fake.local.as_ref().unwrap().lock().unwrap();
        l.pulls[0].phase = "done".into();
        l.models.push(tern_core::models::LocalModelInfo {
            name: "qwen3.5:9b".into(),
            tools: Some(true),
        });
    }
    run_poll(&mut panel, &fake, Task::LocalPoll(o));
    let save = panel.take_task().unwrap();
    let Task::Save {
        draft,
        default_model,
        ..
    } = &save
    else {
        panic!("expected a save, got {save:?}");
    };
    assert_eq!(draft.preset.as_deref(), Some("ollama"));
    assert_eq!(default_model.as_deref(), Some("ollama:qwen3.5:9b"));
    finish(&mut panel, &fake, SetupAction::Run(save));
    assert!(matches!(panel.step, Step::List));
    assert!(panel.changed);
    assert_eq!(fake.default_model().as_deref(), Some("ollama:qwen3.5:9b"));
}

#[test]
fn an_installed_ollama_starts_by_itself_and_a_failed_start_offers_a_retry() {
    let fake = local_fake(true, false);
    fake.local.as_ref().unwrap().lock().unwrap().start_fails = true;
    let mut panel = SetupPanel::open(&fake).unwrap();
    let first = panel.first_task().unwrap();
    run_poll(&mut panel, &fake, first);
    assert_eq!(calls(&fake), ["start"]);
    let Step::Ollama(o) = &panel.step else {
        panic!()
    };
    assert!(o.start_failed);
    assert!(o.error.as_deref().unwrap().contains("11434"));
    // No hammering a start that failed.
    assert!(!poll_opts(panel.take_task()).try_start);
    panel.on_key(press(KeyCode::Char('r')), &fake);
    let Step::Ollama(o) = &panel.step else {
        panic!()
    };
    assert!(!o.start_failed);
}

#[test]
fn the_api_link_opens_the_existing_providers_and_esc_comes_back() {
    let fake = local_fake(false, false);
    let mut panel = SetupPanel::open(&fake).unwrap();
    panel.on_key(press(KeyCode::Char('a')), &fake);
    assert!(matches!(panel.step, Step::Kind { selected: 0 }));
    // The cloud flow is the one it always was.
    typed(&mut panel, &fake, "1");
    assert!(matches!(panel.step, Step::Key(_)));
    panel.on_key(press(KeyCode::Esc), &fake);
    let action = panel.on_key(press(KeyCode::Esc), &fake);
    assert!(matches!(panel.step, Step::Ollama(_)));
    assert!(matches!(action, SetupAction::Run(Task::LocalPoll(_))));
}

#[test]
fn a_model_that_cannot_use_tools_is_not_made_the_default() {
    let fake = local_fake(true, true);
    fake.local
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .models
        .push(tern_core::models::LocalModelInfo {
            name: "llama2".into(),
            tools: Some(false),
        });
    let mut panel = SetupPanel::open(&fake).unwrap();
    let first = panel.first_task().unwrap();
    run_poll(&mut panel, &fake, first);
    assert!(matches!(panel.take_task(), Some(Task::LocalPoll(_))));
}

#[test]
fn without_a_background_agent_first_run_is_the_provider_list_as_before() {
    let fake = Fake {
        needs_setup: true,
        ..Fake::default()
    };
    let panel = SetupPanel::open(&fake).unwrap();
    assert!(matches!(panel.step, Step::Kind { selected: 0 }));
    assert!(panel.first_task().is_none());
}
