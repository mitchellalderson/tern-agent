//! Fleet status in the TUI: the state marker, the header summary and the
//! spend line.
use super::*;

fn info(slug: &str, needs_you: usize, active_runs: usize) -> AgentInfo {
    AgentInfo {
        id: format!("tern:{slug}"),
        slug: slug.into(),
        name: slug.into(),
        needs_you,
        active_runs,
        is_default: false,
        ceiling: None,
        code_auto: false,
    }
}

#[test]
fn agent_state_needs_you_beats_working_beats_idle() {
    use crate::AgentState::*;
    assert_eq!(info("a", 0, 0).state(), Idle);
    assert_eq!(info("a", 0, 2).state(), Working);
    assert_eq!(info("a", 1, 0).state(), NeedsYou);
    assert_eq!(info("a", 3, 2).state(), NeedsYou);
    assert_eq!(NeedsYou.marker(), '◆');
    assert_eq!(Working.marker(), '●');
    assert_eq!(Idle.marker(), '○');
}

#[test]
fn fleet_summary_counts_agents() {
    let list = [
        info("a", 0, 1),
        info("b", 0, 1),
        info("c", 1, 0),
        info("d", 0, 0),
    ];
    assert_eq!(crate::fleet_summary(&list, true), "1 needs you, 2 working");
    assert_eq!(crate::fleet_summary(&list, false), "2 working");
    assert_eq!(
        crate::fleet_summary(&[info("a", 2, 0), info("b", 1, 0)], true),
        "2 need you"
    );
    assert_eq!(crate::fleet_summary(&[info("a", 0, 0)], true), "");
    assert_eq!(crate::fleet_summary(&[], true), "");
    assert_eq!(crate::spend_line(0.314, 0.5), "$0.31 of $0.50 today");
}

#[test]
fn agents_list_marks_each_agents_state() {
    let (mut app, _fake) = new_app();
    submit(&mut app, "/agents");
    let n = notices(&app);
    assert!(n.contains("○ @research  Research"), "{n}");
    assert!(n.contains("◆ @inbox  Inbox  (2 need you)"), "{n}");
}

#[test]
fn status_line_shows_the_fleet_and_todays_spend_when_there_is_any() {
    let (mut app, _fake) = new_app();
    let quiet = screen(&mut app, 120, 20);
    assert!(
        !quiet.contains("working") && !quiet.contains("today"),
        "{quiet}"
    );

    app.fleet = vec![info("a", 0, 1), info("b", 0, 0)];
    app.spend = Some(crate::spend_line(0.31, 0.5));
    let s = screen(&mut app, 120, 20);
    assert!(s.contains("1 working"), "{s}");
    assert!(s.contains("$0.31 of $0.50 today"), "{s}");

    // What waits on the user is already counted, so it isn't said twice.
    app.needs_you = 1;
    app.fleet = vec![info("a", 0, 1), info("b", 1, 0)];
    let s = screen(&mut app, 120, 20);
    assert!(s.contains("1 need you · 1 working"), "{s}");
    assert!(!s.contains("1 needs you"), "{s}");
}
