//! The `/settings` screen against the fake backend.

use super::*;

/// Move the settings cursor onto `key`.
fn goto_setting(app: &mut App, key: &str) {
    for _ in 0..80 {
        if app
            .settings
            .as_ref()
            .and_then(|p| p.current())
            .is_some_and(|s| s.key == key)
        {
            return;
        }
        app.on_key(key_of(KeyCode::Down), Instant::now());
    }
    for _ in 0..80 {
        if app
            .settings
            .as_ref()
            .and_then(|p| p.current())
            .is_some_and(|s| s.key == key)
        {
            return;
        }
        app.on_key(key_of(KeyCode::Up), Instant::now());
    }
    panic!("no setting {key} in this section");
}

fn key_of(code: KeyCode) -> KeyEvent {
    key(code)
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(key(code), Instant::now());
}

fn clear_line(app: &mut App) {
    app.on_key(
        with(KeyCode::Char('u'), KeyModifiers::CONTROL),
        Instant::now(),
    );
}

fn setting_calls(fake: &Fake) -> Vec<(String, Option<serde_json::Value>)> {
    fake.setting_calls.lock().unwrap().clone()
}

#[test]
fn every_section_is_there_and_toggles_and_resets_work() {
    let (mut app, fake) = new_app();
    submit(&mut app, "/settings");
    let shown = screen(&mut app, 110, 34);
    for title in [
        "General",
        "Models & Providers",
        "Secrets",
        "Permissions & Sandbox",
        "Limits & Budgets",
        "Servers",
        "Advanced",
        "About",
    ] {
        assert!(shown.contains(title), "{title}: {shown}");
    }
    assert!(
        app.input.text().is_empty(),
        "the screen takes the keys, not the chat input"
    );

    // 4 jumps to Permissions & Sandbox.
    press(&mut app, KeyCode::Char('4'));
    goto_setting(&mut app, "sandbox.network");
    let shown = screen(&mut app, 110, 34);
    assert!(shown.contains("Network") && shown.contains("applies when tern restarts"));
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        setting_calls(&fake),
        vec![("sandbox.network".into(), Some(serde_json::json!(true)))]
    );
    let shown = screen(&mut app, 110, 34);
    assert!(
        shown.contains("saved sandbox.network; applies when tern restarts"),
        "{shown}"
    );
    assert!(shown.contains("1 saved change applies when tern restarts"));
    assert!(shown.contains('↻') && shown.contains("Safety:"), "{shown}");

    // Reset puts it back to the default, out of the file.
    press(&mut app, KeyCode::Char('r'));
    assert_eq!(setting_calls(&fake).last().unwrap().1, None);
    assert!(!screen(&mut app, 110, 34).contains("saved change applies"));

    // A choice cycles with Left (the default is autonomous).
    goto_setting(&mut app, "autonomy");
    press(&mut app, KeyCode::Left);
    assert_eq!(
        setting_calls(&fake).last().unwrap(),
        &("autonomy".to_owned(), Some(serde_json::json!("supervised")))
    );
    assert!(screen(&mut app, 110, 34).contains("autonomous"));

    press(&mut app, KeyCode::Esc);
    assert!(app.settings.is_none());
}

#[test]
fn numbers_are_checked_and_the_cores_refusals_are_shown() {
    let (mut app, fake) = new_app();
    submit(&mut app, "/settings limits");
    goto_setting(&mut app, "subagents.max_result_chars");
    let shown = screen(&mut app, 110, 34);
    assert!(
        shown.contains("Max result chars") && shown.contains("8000"),
        "{shown}"
    );

    // The typed text is checked as a number before it is sent.
    press(&mut app, KeyCode::Enter);
    clear_line(&mut app);
    type_text(&mut app, "lots");
    press(&mut app, KeyCode::Enter);
    assert!(setting_calls(&fake).is_empty());
    assert!(screen(&mut app, 110, 34).contains("a whole number"));
    clear_line(&mut app);
    type_text(&mut app, "12000");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        setting_calls(&fake),
        vec![(
            "subagents.max_result_chars".into(),
            Some(serde_json::json!(12000))
        )]
    );
    let shown = screen(&mut app, 110, 34);
    assert!(shown.contains("12000") && shown.contains('*'), "{shown}");

    // The core's refusal is shown and nothing changes.
    goto_setting(&mut app, "subagents.max_depth");
    press(&mut app, KeyCode::Enter);
    clear_line(&mut app);
    type_text(&mut app, "9");
    press(&mut app, KeyCode::Enter);
    assert!(screen(&mut app, 110, 34).contains("must be at most 3"));
    // Esc cancels an edit without closing the screen.
    press(&mut app, KeyCode::Esc);
    assert!(app.settings.is_some());

    // An optional number takes a value.
    goto_setting(&mut app, "schedules.usd_per_day");
    press(&mut app, KeyCode::Enter);
    type_text(&mut app, "5.5");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        setting_calls(&fake).last().unwrap().1,
        Some(serde_json::json!(5.5))
    );
}

#[test]
fn rows_open_the_existing_secrets_and_mcp_panels_and_return() {
    let (mut app, _fake) = new_app();
    submit(&mut app, "/settings");
    press(&mut app, KeyCode::Char('3'));
    let shown = screen(&mut app, 110, 34);
    assert!(shown.contains("open"), "{shown}");
    press(&mut app, KeyCode::Enter);
    assert!(app.settings.is_none() && app.secrets.is_some());
    assert!(screen(&mut app, 110, 34).contains("API_KEY"));
    // Closing it comes back to the same section of Settings.
    press(&mut app, KeyCode::Esc);
    assert!(app.secrets.is_none());
    assert_eq!(
        app.settings.as_ref().map(|p| p.section),
        Some(crate::settings::Section::Secrets)
    );

    press(&mut app, KeyCode::Char('6'));
    press(&mut app, KeyCode::Enter);
    assert!(app.mcp.is_some(), "MCP servers is a row of Servers");
    press(&mut app, KeyCode::Esc);
    assert!(app.mcp.is_none());
    assert!(app.settings.is_some());
    press(&mut app, KeyCode::Esc);
    assert!(app.settings.is_none());
}

#[test]
fn search_covers_every_section() {
    let (mut app, fake) = new_app();
    submit(&mut app, "/settings");
    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "timeout");
    press(&mut app, KeyCode::Enter);
    let shown = screen(&mut app, 110, 34);
    assert!(shown.contains("Shell timeout (seconds)"), "{shown}");
    assert!(!shown.contains("Network"), "{shown}");
    // Editing a result works like in its own section.
    goto_setting(&mut app, "sandbox.shell_timeout_secs");
    press(&mut app, KeyCode::Enter);
    clear_line(&mut app);
    type_text(&mut app, "60");
    press(&mut app, KeyCode::Enter);
    assert_eq!(setting_calls(&fake)[0].1, Some(serde_json::json!(60)));
    // Esc clears the search, a second one closes.
    press(&mut app, KeyCode::Esc);
    assert!(app.settings.as_ref().unwrap().query.is_empty());
    press(&mut app, KeyCode::Esc);
    assert!(app.settings.is_none());

    // A section that has an editor of its own answers for it too.
    submit(&mut app, "/settings api key");
    let shown = screen(&mut app, 110, 34);
    assert!(
        shown.contains("Secrets") && shown.contains("open"),
        "{shown}"
    );
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Esc);

    submit(&mut app, "/settings zzzz");
    assert!(screen(&mut app, 110, 34).contains("No setting matches."));
}

#[test]
fn about_lists_where_things_are_and_a_backend_without_settings_says_so() {
    let (mut app, _) = new_app();
    submit(&mut app, "/settings about");
    let shown = screen(&mut app, 110, 34);
    assert!(shown.contains("/nonexistent/tern/config.toml"), "{shown}");
    assert!(shown.contains("no telemetry"), "{shown}");
    assert!(shown.contains("/nonexistent/tern/logs"), "{shown}");
    // Rows that are only information can't be edited.
    press(&mut app, KeyCode::Enter);
    assert!(app.settings.is_some());

    struct Bare;
    #[async_trait]
    impl Backend for Bare {
        fn default_model(&self) -> Option<String> {
            None
        }
        fn check_model(&self, _: &str) -> BackendResult<()> {
            Ok(())
        }
        fn start_run(&self, _: Option<&str>, _: Option<&str>, _: &str) -> BackendResult<Started> {
            Err("no".into())
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
    let mut app = App::new(Arc::new(Bare));
    submit(&mut app, "/settings");
    assert!(app.settings.is_none());
    assert!(screen(&mut app, 100, 20).contains("can't edit settings"));
}
