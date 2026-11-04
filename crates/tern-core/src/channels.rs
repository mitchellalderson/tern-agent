//! Channels: topic timelines over one shared memory, and Home
//! (docs/channels-design.md). The store holds them; this module has the
//! shapes clients see and the rules that don't need the store.

use chrono::{DateTime, Duration, Local};
use serde::{Deserialize, Serialize};

/// Stable author ids. They are what a member id (spec §15) or an agent
/// profile id replaces later, so they never change for an author; the
/// display name (`author`) may.
pub mod author {
    /// The one person who owns this core; becomes the owner's member id.
    pub const OWNER: &str = "owner";
    /// The main agent; becomes its agent profile's id.
    pub const AGENT: &str = "tern";

    /// A sub-agent of the main agent, by its label.
    pub fn subagent(label: &str) -> String {
        format!("{AGENT}/{label}")
    }

    /// A routine, by its schedule id (its title may change).
    pub fn routine(schedule_id: &str) -> String {
        format!("routine:{schedule_id}")
    }
}

/// A post as clients see it (`GET /v1/channels/{id}/posts`, `post_created`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Post {
    pub id: i64,
    pub channel_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// `user`, `agent`, `subagent` or `routine`.
    pub author_kind: String,
    /// Stable identity (see [`author`]).
    pub author_id: String,
    /// What to show.
    pub author: String,
    /// `message`, `answer`, `update`, `needs_you` or `pointer`.
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points_to: Option<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tainted: bool,
    /// Posts replying to this one in a side thread.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub replies: i64,
    /// Unix ms.
    pub created_at: i64,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

impl From<tern_store::PostRow> for Post {
    fn from(p: tern_store::PostRow) -> Self {
        Self {
            id: p.id,
            channel_id: p.channel_id,
            session_id: p.session_id,
            run_id: p.run_id,
            author_kind: p.author_kind,
            author_id: p.author_id,
            author: p.author,
            kind: p.kind,
            text: p.text,
            reply_to: p.reply_to,
            points_to: p.points_to,
            tainted: p.tainted,
            replies: 0,
            created_at: p.created_at,
        }
    }
}

/// A channel slug: `[a-z0-9][a-z0-9-]{0,31}`.
pub fn check_slug(slug: &str) -> Result<(), String> {
    let ok = !slug.is_empty()
        && slug.len() <= 32
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !slug.starts_with('-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "channel name {slug:?} must be 1–32 lowercase letters, digits or dashes, not starting with a dash"
        ))
    }
}

/// The system prompt section for runs in a channel with instructions.
pub fn instructions_section(slug: &str, instructions: &str) -> Option<String> {
    let text = instructions.trim();
    (!text.is_empty()).then(|| {
        format!(
            "\n\n## Channel #{slug}\n\nThis conversation is in the #{slug} channel. The user's instructions for it:\n\n{text}"
        )
    })
}

/// `[home]`: when Home's stream (and each Telegram chat's) is cut into a
/// new segment (docs/channels-design.md §2.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HomeConfig {
    /// A segment idle this long is cut at the next message.
    pub idle_hours: f64,
    /// Local time a new day starts: a segment whose last message is from
    /// before the latest one, and idle for an hour, is cut.
    pub day_starts: ClockTime,
}

impl Default for HomeConfig {
    fn default() -> Self {
        Self {
            idle_hours: 6.0,
            day_starts: ClockTime(4 * 60),
        }
    }
}

/// A local time of day, `HH:MM`, as minutes after midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ClockTime(pub u32);

impl TryFrom<String> for ClockTime {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        crate::quiet::minutes(&s)
            .map(ClockTime)
            .ok_or_else(|| format!("time {s:?}: expected \"HH:MM\" (e.g. \"04:00\")"))
    }
}

impl From<ClockTime> for String {
    fn from(t: ClockTime) -> Self {
        format!("{:02}:{:02}", t.0 / 60, t.0 % 60)
    }
}

/// Why a stream was cut into a new segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutReason {
    /// Idle for `idle_hours`.
    Idle,
    /// A new day began (`day_starts`) since its last message.
    Day,
    /// The user asked for a new topic.
    NewTopic,
}

impl CutReason {
    pub fn as_str(self) -> &'static str {
        match self {
            CutReason::Idle => "idle",
            CutReason::Day => "day",
            CutReason::NewTopic => "new_topic",
        }
    }
}

/// Whether a segment whose last message was at `last` should be cut when
/// the user sends at `now` (§2.6). The caller has already checked that no
/// run is active in it, and handles New topic itself.
pub fn cut_reason(
    cfg: &HomeConfig,
    last: DateTime<Local>,
    now: DateTime<Local>,
) -> Option<CutReason> {
    let idle = now - last;
    if idle.num_seconds() as f64 >= cfg.idle_hours * 3600.0 {
        return Some(CutReason::Idle);
    }
    let today = now.date_naive();
    let mut boundary = crate::quiet::at_local(today, cfg.day_starts.0);
    if boundary > now {
        boundary = crate::quiet::at_local(today - Duration::days(1), cfg.day_starts.0);
    }
    (last < boundary && idle >= Duration::hours(1)).then_some(CutReason::Day)
}

/// Starts the message a `carry` step adds to a new segment.
pub const CARRY_HEADER: &str = "[Continuing from your previous session";

/// What crosses a cut from one segment to the next.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carry {
    /// Unix ms of the previous segment's first and last messages.
    pub from: i64,
    pub to: i64,
    pub summary: String,
    /// Things still open there: runs, sub-agents.
    pub open: Vec<String>,
    /// The last exchanges, oldest first: (the user's text, the reply).
    pub last: Vec<(String, String)>,
}

/// Most bytes of the last exchanges carried verbatim.
pub const CARRY_EXCHANGES_MAX: usize = 2048;

fn clock(ms: i64) -> Option<DateTime<Local>> {
    DateTime::from_timestamp_millis(ms).map(|t| t.with_timezone(&Local))
}

/// "Tue 08:12–18:40", or "Mon 08:12 – Tue 18:40" across days.
pub fn span(from: i64, to: i64) -> String {
    let (Some(a), Some(b)) = (clock(from), clock(to)) else {
        return String::new();
    };
    if a.date_naive() == b.date_naive() {
        format!("{}–{}", a.format("%a %H:%M"), b.format("%H:%M"))
    } else {
        format!("{} – {}", a.format("%a %H:%M"), b.format("%a %H:%M"))
    }
}

impl Carry {
    /// The user-role message the `carry` step appends.
    pub fn render(&self) -> String {
        let mut out = format!(
            "{CARRY_HEADER} ({}). This is a summary; ask with history_search for detail.]",
            span(self.from, self.to)
        );
        if !self.summary.trim().is_empty() {
            out.push_str(&format!("\nSummary: {}", self.summary.trim()));
        }
        if !self.open.is_empty() {
            out.push_str(&format!("\nOpen: {}", self.open.join(" · ")));
        }
        if !self.last.is_empty() {
            out.push_str("\nLast exchange:");
            let mut budget = CARRY_EXCHANGES_MAX;
            for (you, tern) in &self.last {
                for (who, text) in [("you", you), ("tern", tern)] {
                    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    let cut = crate::subagent::cap(&one_line, budget.min(one_line.len()));
                    let cut = cut.lines().next().unwrap_or_default();
                    budget = budget.saturating_sub(cut.len());
                    out.push_str(&format!("\n  {who}: {cut}"));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        for ok in ["finances", "q4-launch", "a", "0", &"x".repeat(32)] {
            assert!(check_slug(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "Finances", "a b", "a_b", "#x", &"x".repeat(33)] {
            assert!(check_slug(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn author_ids_are_stable_names() {
        assert_eq!(author::subagent("research-1"), "tern/research-1");
        assert_eq!(author::routine("abc"), "routine:abc");
    }

    fn at(day: u32, h: u32, m: u32) -> DateTime<Local> {
        let d = chrono::NaiveDate::from_ymd_opt(2026, 3, day).unwrap();
        crate::quiet::at_local(d, h * 60 + m)
    }

    #[test]
    fn segments_are_cut_after_idle_hours_or_a_new_day() {
        let cfg = HomeConfig::default();
        // Active an hour ago, same morning: carry on.
        assert_eq!(cut_reason(&cfg, at(10, 9, 0), at(10, 10, 0)), None);
        // Six idle hours.
        assert_eq!(
            cut_reason(&cfg, at(10, 9, 0), at(10, 15, 0)),
            Some(CutReason::Idle)
        );
        // Past 04:00 since the last message, idle over an hour.
        assert_eq!(
            cut_reason(&cfg, at(10, 2, 0), at(10, 5, 30)),
            Some(CutReason::Day)
        );
        // Past 04:00, but only 40 minutes idle: a late night keeps going.
        assert_eq!(cut_reason(&cfg, at(10, 3, 50), at(10, 4, 30)), None);
        // 23:00 to 01:30: no day boundary yet (it's at 04:00).
        assert_eq!(cut_reason(&cfg, at(9, 23, 0), at(10, 1, 30)), None);
        // Yesterday evening to this morning.
        assert_eq!(cut_reason(&cfg, at(9, 22, 0), at(10, 3, 0)), None);
        assert_eq!(
            cut_reason(&cfg, at(9, 23, 30), at(10, 5, 0)),
            Some(CutReason::Day)
        );
        let cfg = HomeConfig {
            idle_hours: 0.5,
            ..HomeConfig::default()
        };
        assert_eq!(
            cut_reason(&cfg, at(10, 9, 0), at(10, 9, 31)),
            Some(CutReason::Idle)
        );
    }

    #[test]
    fn home_config_parses_times() {
        let c: HomeConfig = toml::from_str("idle_hours = 2\nday_starts = \"05:30\"").unwrap();
        assert_eq!((c.idle_hours, c.day_starts), (2.0, ClockTime(330)));
        assert!(toml::from_str::<HomeConfig>("day_starts = \"25:00\"").is_err());
        assert_eq!(String::from(ClockTime(4 * 60)), "04:00");
    }

    #[test]
    fn carry_renders_summary_open_items_and_the_last_exchanges() {
        let c = Carry {
            from: at(10, 8, 12).timestamp_millis(),
            to: at(10, 18, 40).timestamp_millis(),
            summary: "We sorted invoices.".into(),
            open: vec!["research-1 still running".into()],
            last: vec![(
                "Let's hold the Northwind one.".into(),
                "Holding it.\nI'll remind you Friday.".into(),
            )],
        };
        let text = c.render();
        assert!(text.starts_with(CARRY_HEADER), "{text}");
        assert!(text.contains("(Tue 08:12–18:40)"), "{text}");
        assert!(text.contains("\nSummary: We sorted invoices."));
        assert!(text.contains("\nOpen: research-1 still running"));
        assert!(
            text.ends_with(
                "  you: Let's hold the Northwind one.\n  tern: Holding it. I'll remind you Friday."
            ),
            "{text}"
        );
        let long = Carry {
            last: vec![("x".repeat(5000), "y".repeat(5000))],
            ..Carry::default()
        };
        assert!(long.render().len() < CARRY_EXCHANGES_MAX + 300);
    }

    #[test]
    fn instructions_become_a_prompt_section_only_when_given() {
        assert_eq!(instructions_section("fin", "  "), None);
        let s = instructions_section("fin", "Amounts in EUR.").unwrap();
        assert!(s.contains("## Channel #fin") && s.ends_with("Amounts in EUR."));
    }
}
