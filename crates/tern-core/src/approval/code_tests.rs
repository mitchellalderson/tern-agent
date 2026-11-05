//! `approval::check` for code auto mode against every other level.

use super::*;
use crate::tool::{Effect, Reach, ToolPolicy};

fn gate(autonomy: Autonomy, tainted: bool) -> Gate {
    Gate { tainted, autonomy }
}

/// A shell command the classifier cleared in a verified, offline sandbox.
fn shell_auto() -> ToolPolicy {
    ToolPolicy::new(Effect::Destructive, false)
        .forced(true)
        .reaching(Reach::Workspace)
        .auto_code(true)
}

/// A shell command the classifier (or the network, or the sandbox) refuses.
fn shell_asks() -> ToolPolicy {
    ToolPolicy::new(Effect::Destructive, false)
        .forced(true)
        .reaching(Reach::Workspace)
}

/// A shell command with a sandbox that isn't verified or not there at all
/// (`unverified` / `unbounded` shell state): never auto.
fn shell_unbounded() -> ToolPolicy {
    ToolPolicy::new(Effect::Destructive, false)
        .forced(true)
        .unconfined(true)
}

fn workspace_write() -> ToolPolicy {
    ToolPolicy::granted_write(false).reaching(Reach::Workspace)
}

fn outside_write() -> ToolPolicy {
    ToolPolicy::granted_write(false)
        .reaching(Reach::Workspace)
        .forced(true)
}

fn is_ask(c: Check) -> bool {
    matches!(c, Check::Ask { .. })
}

const LEVELS: [Autonomy; 5] = [
    Autonomy::ReadOnly,
    Autonomy::Supervised,
    Autonomy::Code,
    Autonomy::Autonomous,
    Autonomy::Yolo,
];

#[test]
fn the_ladder_is_ordered_and_named() {
    assert!(Autonomy::Supervised < Autonomy::Code && Autonomy::Code < Autonomy::Autonomous);
    assert_eq!(Autonomy::parse("code"), Some(Autonomy::Code));
    assert_eq!(Autonomy::Code.as_str(), "code");
    assert_eq!(Autonomy::of_run(Some("code")), Autonomy::Code);
    let json = serde_json::to_string(&Autonomy::Code).unwrap();
    assert_eq!(json, "\"code\"");
}

#[test]
fn a_cleared_command_runs_at_code_and_above_never_below() {
    let p = shell_auto();
    for l in LEVELS {
        let c = check("shell", &p, false, gate(l, false));
        match l {
            Autonomy::ReadOnly => assert_eq!(c, Check::ReadOnly, "{l}"),
            Autonomy::Supervised => assert!(is_ask(c), "{l}: {c:?}"),
            _ => assert_eq!(c, Check::Allow, "{l}: {c:?}"),
        }
    }
}

#[test]
fn a_command_the_classifier_refuses_asks_at_every_attended_level() {
    let p = shell_asks();
    for l in [Autonomy::Supervised, Autonomy::Code, Autonomy::Autonomous] {
        assert!(
            is_ask(check("shell", &p, false, gate(l, false))),
            "{l} must ask"
        );
    }
    // yolo is the only level that waves it through, as before.
    assert_eq!(
        check("shell", &p, false, gate(Autonomy::Yolo, false)),
        Check::Allow
    );
}

#[test]
fn an_unverified_or_unbounded_shell_asks_exactly_as_supervised() {
    let p = shell_unbounded();
    let sup = check("shell", &p, false, gate(Autonomy::Supervised, false));
    assert_eq!(
        check("shell", &p, false, gate(Autonomy::Code, false)),
        sup,
        "code is no looser than supervised without a sandbox"
    );
    assert!(matches!(sup, Check::Ask { always: false, .. }));
    // Even a policy that (wrongly) claims both is not waved through.
    let both = shell_unbounded().auto_code(true);
    assert!(is_ask(check(
        "shell",
        &both,
        false,
        gate(Autonomy::Code, false)
    )));
}

#[test]
fn a_tainted_run_never_gets_the_mode() {
    let p = shell_auto();
    for l in [Autonomy::Supervised, Autonomy::Code, Autonomy::Autonomous] {
        match check("shell", &p, false, gate(l, true)) {
            Check::Ask { always, .. } => assert!(!always, "{l}"),
            c => panic!("{l}: tainted shell must ask, got {c:?}"),
        }
    }
    // File writes lose their grants in a tainted run too.
    for l in [Autonomy::Supervised, Autonomy::Code] {
        assert!(is_ask(check(
            "write_file",
            &workspace_write(),
            true,
            gate(l, true)
        )));
    }
}

#[test]
fn file_writes_inside_the_workspace_are_unasked_and_outside_ones_ask() {
    for l in [Autonomy::Supervised, Autonomy::Code, Autonomy::Autonomous] {
        assert_eq!(
            check("write_file", &workspace_write(), false, gate(l, false)),
            Check::Allow,
            "{l}"
        );
        assert!(
            is_ask(check("write_file", &outside_write(), false, gate(l, false))),
            "{l}: a write that needs approval still asks"
        );
    }
}

#[test]
fn nothing_else_changes_at_code() {
    // Network fetches, connectors and unknown writes behave as supervised.
    let connector = ToolPolicy::new(Effect::Write, false);
    let money = {
        let mut p = ToolPolicy::new(Effect::Write, false);
        p.forbidden = Some("money_movement");
        p
    };
    assert_eq!(
        check(
            "lua__x__send",
            &connector,
            false,
            gate(Autonomy::Code, false)
        ),
        check(
            "lua__x__send",
            &connector,
            false,
            gate(Autonomy::Supervised, false)
        ),
    );
    for l in LEVELS {
        assert_eq!(
            check("pay", &money, false, gate(l, false)),
            Check::Forbid("money_movement"),
            "{l}"
        );
    }
    // Reads never ask, in code mode as anywhere.
    let read = ToolPolicy::new(Effect::Read, true);
    assert_eq!(
        check("read_file", &read, false, gate(Autonomy::Code, false)),
        Check::Allow
    );
    // A read-only run refuses writes and commands, whatever else is set.
    assert_eq!(
        check(
            "shell",
            &shell_auto(),
            false,
            gate(Autonomy::ReadOnly, false)
        ),
        Check::ReadOnly
    );
}

#[test]
fn code_mode_never_offers_an_always_for_a_command() {
    // A command the mode doesn't cover: the supervised prompt, unchanged.
    let c = check("shell", &shell_asks(), false, gate(Autonomy::Code, false));
    assert_eq!(
        c,
        check(
            "shell",
            &shell_asks(),
            false,
            gate(Autonomy::Supervised, false)
        )
    );
}

#[test]
fn the_effective_level_follows_the_opt_in_and_the_ceiling() {
    use Autonomy::*;
    let eff = effective_autonomy;
    // Opted in, interactive: lifted to code, even under a global supervised.
    assert_eq!(eff(Supervised, None, None, true, true), Code);
    assert_eq!(eff(Supervised, None, Some(Code), true, true), Code);
    assert_eq!(eff(Supervised, None, Some(Yolo), true, true), Code);
    // Never silent: not opted in, no lift.
    assert_eq!(eff(Supervised, None, Some(Yolo), false, true), Supervised);
    // A ceiling below code beats the flag.
    assert_eq!(
        eff(Supervised, None, Some(Supervised), true, true),
        Supervised
    );
    assert_eq!(eff(Supervised, None, Some(ReadOnly), true, true), ReadOnly);
    // Global readonly always wins.
    assert_eq!(eff(ReadOnly, None, None, true, true), ReadOnly);
    // A routine without its own level, or any non-interactive run: plain.
    assert_eq!(eff(Supervised, None, None, true, false), Supervised);
    // A routine that sets code itself gets it, under the ceiling.
    assert_eq!(eff(Supervised, Some(Code), None, true, false), Code);
    assert_eq!(
        eff(Supervised, Some(Code), Some(Supervised), true, false),
        Supervised
    );
    // A level above code is never lowered by the flag.
    assert_eq!(eff(Autonomous, None, None, true, true), Autonomous);
    assert_eq!(eff(Yolo, None, Some(Autonomous), true, true), Autonomous);
}

#[test]
fn a_delegate_never_exceeds_its_ceiling_and_reaches_code_only_by_its_own_opt_in() {
    use Autonomy::*;
    let d = delegate_autonomy;
    // A parent at code, a delegate whose ceiling is supervised: supervised.
    assert_eq!(d(Code, Some(Supervised), false), Supervised);
    assert_eq!(d(Code, Some(Supervised), true), Supervised);
    // Its own opt-in lifts it even when the parent is supervised.
    assert_eq!(d(Supervised, Some(Code), true), Code);
    assert_eq!(d(Supervised, None, true), Code);
    // No opt-in: it keeps its parent's level (code only if the parent has it).
    assert_eq!(d(Code, None, false), Code);
    assert_eq!(d(Supervised, Some(Code), false), Supervised);
    // A read-only parent keeps its children read-only.
    assert_eq!(d(ReadOnly, None, true), ReadOnly);
    assert_eq!(d(Yolo, Some(Autonomous), false), Autonomous);
}

#[test]
fn the_disclaimer_says_what_is_and_is_not_confined() {
    let t = code_auto_disclaimer("@bug-fixer", "~/proj", Some("supervised"));
    for needle in [
        "not fully sandboxed",
        "@bug-fixer",
        "~/proj",
        "without asking you each time",
        "Pushing code, installing packages",
        "including uncommitted work",
        "runs on your machine",
        "not a virtual machine",
        "cannot confirm the sandbox",
        "pauses auto mode",
        "from supervised to code",
    ] {
        assert!(t.contains(needle), "missing {needle:?} in:\n{t}");
    }
    assert!(!code_auto_disclaimer("@a", "~/p", None).contains("raises"));
}

#[test]
fn the_new_outcome_runs_and_serializes() {
    assert!(ApprovalOutcome::ApprovedByMode.runs());
    assert_eq!(
        serde_json::to_string(&ApprovalOutcome::ApprovedByMode).unwrap(),
        "\"approved_by_mode\""
    );
}
