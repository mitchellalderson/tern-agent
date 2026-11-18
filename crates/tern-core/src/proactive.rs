//! Proactive help, host side (docs/innbot-learnings.md P1, P2, P5, P6, P7).
//!
//! The host decides, not the model: every feature here is plain code over
//! what the store already knows, with a switch (`[proactive]`, default on)
//! and a hard cap. A dismissible card is the only way any of it reaches the
//! user. The rules live here as pure functions so they are testable without
//! a daemon.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// `[proactive]` in config.toml: one switch per feature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProactiveConfig {
    /// P5: a daily check of routines, connector sign-ins and waits.
    pub heartbeat: bool,
    /// P6: after repeated failures or empty reports, the owner proposes a fix.
    pub fix_routines: bool,
    /// P7: offer "Always allow this" after repeated "Allow once".
    pub learn_approvals: bool,
    /// P1: suggest a routine after the same ask three times.
    pub suggest_routines: bool,
    /// P2: "While you were away" on the first open of the day.
    pub away_brief: bool,
}

impl Default for ProactiveConfig {
    fn default() -> Self {
        Self {
            heartbeat: true,
            fix_routines: true,
            learn_approvals: true,
            suggest_routines: true,
            away_brief: true,
        }
    }
}

const HOUR: i64 = 3_600_000;
pub const DAY: i64 = 24 * HOUR;

/// Card kinds, as stored.
pub mod kind {
    pub const HEARTBEAT: &str = "heartbeat";
    pub const ROUTINE_FIX: &str = "routine_fix";
    pub const ROUTINE_SUGGESTION: &str = "routine_suggestion";
    pub const AWAY_BRIEF: &str = "away_brief";
}

// ---- P7: learn from approvals ----

/// "Allow once" answers (same agent, tool and target) before the card offers
/// "Always allow this".
pub const LEARN_TAPS: u32 = 3;
/// The window those answers count in.
pub const LEARN_WINDOW_MS: i64 = 14 * DAY;

/// Whether `taps` earlier "Allow once" answers earn the always offer.
pub fn always_learned(taps: u32) -> bool {
    taps >= LEARN_TAPS
}

// ---- P6: self-healing routines ----

/// How a routine's run ended, for [`routine_trouble`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEnd {
    /// Reported something.
    Reported,
    /// Finished with nothing to say (blank or `NO_REPLY`).
    Empty,
    Failed,
}

/// Failures in a row that make a routine worth a look.
pub const FIX_FAILURES: usize = 2;
/// Empty reports in a row that make one worth a look.
pub const FIX_EMPTY: usize = 3;
/// One fix proposal per routine per week.
pub const FIX_WINDOW_MS: i64 = 7 * DAY;
/// And at most this many across all routines per day.
pub const FIX_PER_DAY: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trouble {
    Failing(usize),
    Empty(usize),
}

impl Trouble {
    /// "failed 2 runs in a row".
    pub fn say(self) -> String {
        match self {
            Trouble::Failing(n) => format!("failed its last {n} runs"),
            Trouble::Empty(n) => format!("reported nothing in its last {n} runs"),
        }
    }
}

/// The trouble at the end of `runs` (oldest first), if any: two failures or
/// three empty reports in a row.
pub fn routine_trouble(runs: &[RunEnd]) -> Option<Trouble> {
    let streak = |want: RunEnd| runs.iter().rev().take_while(|r| **r == want).count();
    let failed = streak(RunEnd::Failed);
    if failed >= FIX_FAILURES {
        return Some(Trouble::Failing(failed));
    }
    let empty = streak(RunEnd::Empty);
    (empty >= FIX_EMPTY).then_some(Trouble::Empty(empty))
}

/// The message that starts the fix investigation. The agent may read, never
/// change: the proposal reaches the user as a card.
pub fn fix_prompt(routine_title: &str, routine_id: &str, trouble: Trouble) -> String {
    format!(
        "Routine \"{routine_title}\" ({routine_id}) {}. Look at its recent runs and work out \
         why. Do not change the routine. Reply with a short diagnosis and one proposed fix \
         (new prompt, schedule or connector change) in plain words, at most six lines.",
        trouble.say()
    )
}

// ---- P1: suggest a routine ----

/// Similar asks (this one included) in the window before a routine is suggested.
pub const SUGGEST_ASKS: usize = 3;
pub const SUGGEST_WINDOW_MS: i64 = 14 * DAY;
/// At most one routine suggestion a day, whatever the switch.
pub const SUGGEST_PER_DAY: u32 = 1;

const STOP: &[&str] = &[
    "the", "a", "an", "and", "or", "to", "of", "for", "in", "on", "at", "it", "is", "me", "my",
    "can", "you", "please", "could", "would", "with", "this", "that", "from", "do", "be", "i",
    "we", "us", "our", "your", "any", "some", "now", "just", "again", "as", "so", "if", "up",
];

/// The content words of an ask: lowercase, no stop words, no digits-only,
/// light plural stripping.
pub fn tokens(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.len() > 2 && !STOP.contains(&w.as_str()))
        .filter(|w| w.chars().any(char::is_alphabetic))
        .map(|w| match w.strip_suffix('s') {
            Some(s) if s.len() > 3 && !w.ends_with("ss") => s.to_owned(),
            _ => w,
        })
        .collect()
}

/// Two asks are alike when their content words overlap by half or more.
/// Cheap and host-side; [`SimilarAsks`] is where a decision model plugs in.
pub fn alike(a: &BTreeSet<String>, b: &BTreeSet<String>) -> bool {
    if a.len() < 2 || b.len() < 2 {
        return false;
    }
    let shared = a.intersection(b).count() as f64;
    let total = a.union(b).count() as f64;
    shared / total >= 0.5
}

/// How many of `earlier` are like `current`, plus `current` itself.
pub fn similar_asks(current: &str, earlier: &[String]) -> usize {
    let cur = tokens(current);
    1 + earlier.iter().filter(|e| alike(&cur, &tokens(e))).count()
}

/// Whether a run's ask is worth counting at all: a short "yes" or "thanks"
/// is not a kind of work.
pub fn countable_ask(text: &str) -> bool {
    tokens(text).len() >= 2
}

/// A stable key for "the same ask": its sorted content words, cut. Two cards
/// for one signature are never made in the window.
pub fn ask_signature(text: &str) -> String {
    tokens(text)
        .into_iter()
        .take(6)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The yes/no put to the decision model (`Runtime::decide`) once the host
/// finds three alike asks: is this one repeatable job?
pub const REPEAT_QUESTION: &str = "Are these asks the same repeatable job that could run on a \
     schedule (not three different one-off requests)?";
/// At or above this the answer counts as yes. Unanswered is not a no: the
/// host's own count stands, since the card is cheap and dismissible.
pub const REPEAT_THRESHOLD: f64 = 0.5;

// ---- P2: while you were away ----

/// Hours away before the first open of the day gets a brief.
pub const AWAY_HOURS: i64 = 8;

/// Whether coming back after `gap_ms` earns a brief, given the local day
/// of the last brief (`None`: never) and today's.
pub fn away_due(gap_ms: i64, last_brief_day: Option<&str>, today: &str) -> bool {
    gap_ms >= AWAY_HOURS * HOUR && last_brief_day != Some(today)
}

// ---- P5: heartbeat ----

/// One heartbeat per agent per day.
pub const HEARTBEAT_EVERY_MS: i64 = DAY;
/// A wait this late past its next check is overdue (the poller missed it).
pub const WAIT_OVERDUE_MS: i64 = HOUR;

/// The card text for heartbeat `findings`: short, one line each, at most
/// five, with the rest counted.
pub fn heartbeat_body(findings: &[String]) -> String {
    let mut lines: Vec<String> = findings.iter().take(5).map(|f| format!("• {f}")).collect();
    if findings.len() > 5 {
        lines.push(format!("…and {} more.", findings.len() - 5));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use RunEnd::*;

    #[test]
    fn switches_default_on() {
        let c = ProactiveConfig::default();
        assert!(c.heartbeat && c.fix_routines && c.learn_approvals);
        assert!(c.suggest_routines && c.away_brief);
    }

    #[test]
    fn always_is_learned_after_three() {
        assert!(!always_learned(2));
        assert!(always_learned(3));
    }

    #[test]
    fn trouble_is_two_failures_or_three_empties_in_a_row() {
        assert_eq!(routine_trouble(&[Reported, Failed]), None);
        assert_eq!(
            routine_trouble(&[Reported, Failed, Failed]),
            Some(Trouble::Failing(2))
        );
        assert_eq!(routine_trouble(&[Failed, Failed, Reported]), None, "mended");
        assert_eq!(routine_trouble(&[Empty, Empty]), None);
        assert_eq!(
            routine_trouble(&[Reported, Empty, Empty, Empty]),
            Some(Trouble::Empty(3))
        );
        assert_eq!(routine_trouble(&[Empty, Empty, Failed, Empty, Empty]), None);
    }

    #[test]
    fn like_asks_are_counted_and_unlike_ones_are_not() {
        let earlier = [
            "Summarise my unread emails from today".to_owned(),
            "can you summarise the unread emails I got today".to_owned(),
            "book a flight to Lisbon".to_owned(),
        ];
        assert_eq!(
            similar_asks("Please summarise today's unread emails", &earlier),
            3
        );
        assert_eq!(similar_asks("Book a table for two", &earlier), 1);
        assert!(!countable_ask("yes"));
        assert!(!countable_ask("thanks!"));
        assert!(countable_ask("check the build status"));
    }

    #[test]
    fn a_signature_ignores_word_order_and_filler() {
        assert_eq!(
            ask_signature("summarise the unread emails"),
            ask_signature("Emails unread, please summarise")
        );
    }

    #[test]
    fn a_brief_needs_eight_hours_and_a_new_day() {
        let h = 3_600_000;
        assert!(away_due(9 * h, None, "2026-11-18"));
        assert!(!away_due(7 * h, None, "2026-11-18"));
        assert!(!away_due(9 * h, Some("2026-11-18"), "2026-11-18"));
        assert!(away_due(9 * h, Some("2026-11-17"), "2026-11-18"));
    }

    #[test]
    fn the_heartbeat_body_is_short() {
        let f: Vec<String> = (1..=7).map(|n| format!("thing {n}")).collect();
        let body = heartbeat_body(&f);
        assert_eq!(body.lines().count(), 6);
        assert!(body.ends_with("…and 2 more."));
    }
}
