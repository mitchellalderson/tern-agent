//! `/autonomy`: code auto mode is opted into per agent, after reading what it
//! does and does not protect, and the status line says when it is on.
use super::*;

fn calls(fake: &Fake) -> Vec<(String, bool)> {
    fake.code_calls.lock().unwrap().clone()
}

#[test]
fn code_shows_the_disclaimer_first_and_changes_nothing() {
    let (mut app, fake) = new_app();
    submit(&mut app, "/autonomy code");
    let n = notices(&app);
    for needle in [
        "not fully sandboxed",
        "without asking you each time",
        "including uncommitted work",
        "not a virtual machine",
        "/autonomy code yes",
    ] {
        assert!(n.contains(needle), "missing {needle:?} in {n}");
    }
    assert!(calls(&fake).is_empty(), "nothing is turned on by reading");
    assert!(!app.code_auto);
}

#[test]
fn yes_turns_it_on_and_the_status_line_says_so() {
    let (mut app, fake) = new_app();
    submit(&mut app, "/autonomy code yes");
    assert_eq!(calls(&fake), [("tern".to_owned(), true)]);
    assert!(app.code_auto, "the status line follows the agent");
    let backend: Arc<dyn Backend> = fake.clone();
    let _ = backend;
    submit(&mut app, "/autonomy off");
    assert_eq!(
        calls(&fake),
        [("tern".to_owned(), true), ("tern".to_owned(), false)]
    );
    assert!(!app.code_auto);
}

#[test]
fn a_starter_ceiling_below_code_is_named_and_a_read_only_agent_cannot_opt_in() {
    let (mut app, fake) = new_app();
    fake.ceilings
        .lock()
        .unwrap()
        .insert("tern".into(), "supervised".into());
    submit(&mut app, "/autonomy code");
    assert!(
        notices(&app).contains("raises @tern's ceiling from supervised to code"),
        "{}",
        notices(&app)
    );
    fake.ceilings
        .lock()
        .unwrap()
        .insert("tern".into(), "readonly".into());
    submit(&mut app, "/autonomy code yes");
    assert!(calls(&fake).is_empty(), "{:?}", calls(&fake));
}

#[test]
fn autonomy_with_no_argument_reports_the_state() {
    let (mut app, _fake) = new_app();
    submit(&mut app, "/autonomy");
    let n = notices(&app);
    assert!(n.contains("code auto mode off"), "{n}");
}
