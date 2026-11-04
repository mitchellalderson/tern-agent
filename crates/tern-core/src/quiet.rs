//! Quiet hours and the daily windows schedule budgets count in
//! (docs/personal-agent-tooling-spec.md §4.1, §8.3), in local time.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Duration, Local, NaiveDate, NaiveTime, TimeZone, Timelike};
use serde::{Deserialize, Serialize};

/// What a scheduled run's final answer says to deliver nothing.
pub const NO_REPLY: &str = "NO_REPLY";

/// Whether a final answer asks for nothing to be delivered.
pub fn is_no_reply(final_text: &str) -> bool {
    final_text.trim() == NO_REPLY
}

/// A daily window, `HH:MM-HH:MM` in local time (it may cross midnight),
/// or `none`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum QuietHours {
    None,
    /// Minutes after midnight; `start == end` is never quiet.
    Window {
        start: u32,
        end: u32,
    },
}

impl Default for QuietHours {
    fn default() -> Self {
        Self::Window {
            start: 22 * 60,
            end: 7 * 60 + 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("quiet hours {0:?}: expected \"HH:MM-HH:MM\" (e.g. \"22:00-07:30\") or \"none\"")]
pub struct QuietHoursError(String);

fn minutes(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    if !(1..=2).contains(&h.len()) || m.len() != 2 {
        return None;
    }
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

impl FromStr for QuietHours {
    type Err = QuietHoursError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        if t.eq_ignore_ascii_case("none") || t.eq_ignore_ascii_case("off") {
            return Ok(Self::None);
        }
        // Accept the en dash people (and the spec) write.
        let (a, b) = t
            .split_once('-')
            .or_else(|| t.split_once('–'))
            .ok_or_else(|| QuietHoursError(s.to_owned()))?;
        match (minutes(a), minutes(b)) {
            (Some(start), Some(end)) => Ok(Self::Window { start, end }),
            _ => Err(QuietHoursError(s.to_owned())),
        }
    }
}

impl TryFrom<String> for QuietHours {
    type Error = QuietHoursError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<QuietHours> for String {
    fn from(q: QuietHours) -> Self {
        q.to_string()
    }
}

impl fmt::Display for QuietHours {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Window { start, end } => write!(
                f,
                "{:02}:{:02}-{:02}:{:02}",
                start / 60,
                start % 60,
                end / 60,
                end % 60
            ),
        }
    }
}

impl QuietHours {
    /// When a delivery made at `now` may go out: `None` if now isn't
    /// quiet, else the end of the window `now` is in.
    pub fn release_at(&self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        let Self::Window { start, end } = *self else {
            return None;
        };
        let t = now.hour() * 60 + now.minute();
        let today = now.date_naive();
        let end_on = |day: NaiveDate| at_local(day, end);
        if start < end {
            (start..end).contains(&t).then(|| end_on(today))
        } else if start > end {
            if t >= start {
                Some(end_on(today + Duration::days(1)))
            } else if t < end {
                Some(end_on(today))
            } else {
                None
            }
        } else {
            None
        }
    }
}

/// `day` at `minutes` past midnight, local. A time in a DST gap moves to
/// the first valid minute after it.
fn at_local(day: NaiveDate, minutes: u32) -> DateTime<Local> {
    let mut t = day.and_time(NaiveTime::MIN) + Duration::minutes(i64::from(minutes));
    for _ in 0..180 {
        if let Some(d) = Local.from_local_datetime(&t).earliest() {
            return d;
        }
        t += Duration::minutes(1);
    }
    Local::now()
}

/// The start of `now`'s local day (budgets reset there).
pub fn start_of_day(now: DateTime<Local>) -> DateTime<Local> {
    at_local(now.date_naive(), 0)
}

/// The start of the next local day.
pub fn next_day(now: DateTime<Local>) -> DateTime<Local> {
    at_local(now.date_naive() + Duration::days(1), 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(h: u32, m: u32) -> DateTime<Local> {
        let d = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        at_local(d, h * 60 + m)
    }

    #[test]
    fn parses_and_prints() {
        let q: QuietHours = "22:00-07:30".parse().unwrap();
        assert_eq!(q, QuietHours::default());
        assert_eq!(q.to_string(), "22:00-07:30");
        assert_eq!("22:00–07:30".parse::<QuietHours>().unwrap(), q);
        assert_eq!("None".parse::<QuietHours>().unwrap(), QuietHours::None);
        for bad in [
            "",
            "22-07",
            "25:00-07:00",
            "22:00",
            "22:60-01:00",
            "22:000-1:00",
        ] {
            assert!(bad.parse::<QuietHours>().is_err(), "{bad}");
        }
    }

    #[test]
    fn window_across_midnight_releases_at_its_end() {
        let q = QuietHours::default();
        assert_eq!(q.release_at(at(21, 59)), None);
        assert_eq!(q.release_at(at(22, 0)), Some(at(7, 30) + Duration::days(1)));
        assert_eq!(q.release_at(at(3, 0)), Some(at(7, 30)));
        assert_eq!(q.release_at(at(7, 30)), None);
        assert_eq!(QuietHours::None.release_at(at(23, 0)), None);
    }

    #[test]
    fn window_within_a_day() {
        let q: QuietHours = "12:00-13:00".parse().unwrap();
        assert_eq!(q.release_at(at(12, 30)), Some(at(13, 0)));
        assert_eq!(q.release_at(at(13, 0)), None);
        assert_eq!(
            "09:00-09:00"
                .parse::<QuietHours>()
                .unwrap()
                .release_at(at(9, 0)),
            None
        );
    }

    #[test]
    fn no_reply_is_exact_after_trimming() {
        assert!(is_no_reply("  NO_REPLY\n"));
        assert!(!is_no_reply("NO_REPLY."));
        assert!(!is_no_reply("no_reply"));
    }

    #[test]
    fn days() {
        assert_eq!(start_of_day(at(15, 0)), at(0, 0));
        assert_eq!(next_day(at(15, 0)), at(0, 0) + Duration::days(1));
    }
}
