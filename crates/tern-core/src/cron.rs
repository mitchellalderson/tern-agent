//! Standard 5-field cron expressions, evaluated in local time.
//!
//! `minute hour day-of-month month day-of-week`, Vixie semantics: `*`,
//! lists, ranges, steps, month and day names, `0` and `7` both Sunday, and
//! when both day fields are restricted a day matching either one fires.
//! Also `@hourly`, `@daily`/`@midnight`, `@weekly`, `@monthly`,
//! `@yearly`/`@annually`.
//!
//! Hand-rolled instead of the `cron` crate, whose day-of-week numbering
//! starts at Sunday = 1 (so `1-5` means Sunday–Thursday) and which needs a
//! seconds field.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    source: String,
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
    day_star: bool,
    weekday_star: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid cron expression {expr:?}: {reason}")]
pub struct CronError {
    pub expr: String,
    pub reason: String,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

fn parse_field(
    field: &str,
    min: u32,
    max: u32,
    names: &[&str],
    name_base: u32,
) -> Result<(u64, bool), String> {
    let value = |s: &str| -> Result<u32, String> {
        let lower = s.to_ascii_lowercase();
        if let Some(i) = names.iter().position(|n| *n == lower) {
            return Ok(i as u32 + name_base);
        }
        s.parse::<u32>()
            .map_err(|_| format!("{s:?} is not a number"))
    };
    let mut bits = 0u64;
    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().map_err(|_| format!("bad step {s:?}"))?),
            None => (part, 1),
        };
        if step == 0 {
            return Err("step must be at least 1".into());
        }
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (value(a)?, value(b)?)
        } else {
            let v = value(range)?;
            // `5/10` means 5, 15, 25, ...
            (v, if part.contains('/') { max } else { v })
        };
        if lo < min || hi > max || lo > hi {
            return Err(format!("{part:?} is outside {min}-{max}"));
        }
        let mut v = lo;
        while v <= hi {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok((bits, field == "*"))
}

impl FromStr for Cron {
    type Err = CronError;

    fn from_str(expr: &str) -> Result<Self, Self::Err> {
        let err = |reason: String| CronError {
            expr: expr.to_owned(),
            reason,
        };
        let expanded = match expr.trim() {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            other => other,
        };
        let fields: Vec<&str> = expanded.split_whitespace().collect();
        let [m, h, dom, mon, dow] = fields.as_slice() else {
            return Err(err(format!(
                "expected 5 fields (minute hour day month weekday), got {}",
                fields.len()
            )));
        };
        let (minutes, _) =
            parse_field(m, 0, 59, &[], 0).map_err(|e| err(format!("minute: {e}")))?;
        let (hours, _) = parse_field(h, 0, 23, &[], 0).map_err(|e| err(format!("hour: {e}")))?;
        let (days, day_star) =
            parse_field(dom, 1, 31, &[], 0).map_err(|e| err(format!("day of month: {e}")))?;
        let (months, _) =
            parse_field(mon, 1, 12, &MONTHS, 1).map_err(|e| err(format!("month: {e}")))?;
        let (mut weekdays, weekday_star) =
            parse_field(dow, 0, 7, &DAYS, 0).map_err(|e| err(format!("day of week: {e}")))?;
        if weekdays & (1 << 7) != 0 {
            weekdays = (weekdays & !(1 << 7)) | 1;
        }
        Ok(Self {
            source: expr.trim().to_owned(),
            minutes,
            hours,
            days,
            months,
            weekdays,
            day_star,
            weekday_star,
        })
    }
}

impl fmt::Display for Cron {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

impl Cron {
    fn day_matches(&self, d: NaiveDate) -> bool {
        let dom = self.days & (1 << d.day()) != 0;
        let dow = self.weekdays & (1 << d.weekday().num_days_from_sunday()) != 0;
        match (self.day_star, self.weekday_star) {
            (false, false) => dom || dow,
            _ => dom && dow,
        }
    }

    /// The first matching minute strictly after `after`, in local time.
    pub fn next_after(&self, after: DateTime<Local>) -> Option<DateTime<Local>> {
        self.next_after_in(&Local, after)
    }

    pub fn next_after_in<Tz: TimeZone>(
        &self,
        tz: &Tz,
        after: DateTime<Tz>,
    ) -> Option<DateTime<Tz>> {
        let start = after.naive_local().with_second(0)?.with_nanosecond(0)? + Duration::minutes(1);
        let mut t: NaiveDateTime = start;
        // Bounded: any valid expression matches within a few years (Feb 29).
        let limit = start + Duration::days(366 * 5);
        while t < limit {
            if self.months & (1 << t.month()) == 0 {
                let (y, m) = if t.month() == 12 {
                    (t.year() + 1, 1)
                } else {
                    (t.year(), t.month() + 1)
                };
                t = NaiveDate::from_ymd_opt(y, m, 1)?.and_hms_opt(0, 0, 0)?;
                continue;
            }
            if !self.day_matches(t.date()) {
                t = (t.date() + Duration::days(1)).and_hms_opt(0, 0, 0)?;
                continue;
            }
            if self.hours & (1 << t.hour()) == 0 {
                t = t.with_minute(0)? + Duration::hours(1);
                continue;
            }
            if self.minutes & (1 << t.minute()) == 0 {
                t += Duration::minutes(1);
                continue;
            }
            // Times inside a DST gap don't exist; skip them.
            if let Some(local) = tz.from_local_datetime(&t).earliest() {
                return Some(local);
            }
            t += Duration::minutes(1);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Weekday};

    fn at(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).unwrap()
    }

    fn next(expr: &str, from: &str) -> String {
        let c: Cron = expr.parse().unwrap();
        let t = at(from);
        c.next_after_in(t.offset(), t)
            .unwrap()
            .format("%Y-%m-%d %H:%M %a")
            .to_string()
    }

    #[test]
    fn weekdays_at_eight() {
        // 2026-11-06 is a Friday.
        assert_eq!(
            next("0 8 * * 1-5", "2026-11-06T07:59:00+00:00"),
            "2026-11-06 08:00 Fri"
        );
        assert_eq!(
            next("0 8 * * 1-5", "2026-11-06T08:00:00+00:00"),
            "2026-11-09 08:00 Mon"
        );
        assert_eq!(
            next("0 8 * * mon-fri", "2026-11-07T12:00:00+00:00"),
            "2026-11-09 08:00 Mon"
        );
        let c: Cron = "0 8 * * 1-5".parse().unwrap();
        let mut t = at("2026-11-01T00:00:00+00:00");
        for _ in 0..10 {
            t = c.next_after_in(t.offset(), t).unwrap();
            assert!(!matches!(t.weekday(), Weekday::Sat | Weekday::Sun), "{t}");
        }
    }

    #[test]
    fn steps_lists_names_and_sunday_seven() {
        assert_eq!(
            next("*/15 * * * *", "2026-11-06T10:07:30+00:00"),
            "2026-11-06 10:15 Fri"
        );
        assert_eq!(
            next("5/20 * * * *", "2026-11-06T10:26:00+00:00"),
            "2026-11-06 10:45 Fri"
        );
        assert_eq!(
            next("0 9,17 * * *", "2026-11-06T09:00:00+00:00"),
            "2026-11-06 17:00 Fri"
        );
        assert_eq!(
            next("0 0 * * 7", "2026-11-06T00:00:00+00:00"),
            "2026-11-08 00:00 Sun"
        );
        assert_eq!(
            next("0 0 1 jan *", "2026-11-06T00:00:00+00:00"),
            "2027-01-01 00:00 Fri"
        );
        assert_eq!(
            next("@daily", "2026-11-06T23:59:00+00:00"),
            "2026-11-07 00:00 Sat"
        );
        assert_eq!(
            next("0 12 29 2 *", "2026-03-01T00:00:00+00:00"),
            "2028-02-29 12:00 Tue"
        );
    }

    #[test]
    fn day_of_month_or_day_of_week_when_both_restricted() {
        // The 13th, or any Friday.
        assert_eq!(
            next("0 0 13 * 5", "2026-11-07T00:00:00+00:00"),
            "2026-11-13 00:00 Fri"
        );
        assert_eq!(
            next("0 0 13 * 1", "2026-11-07T00:00:00+00:00"),
            "2026-11-09 00:00 Mon"
        );
    }

    #[test]
    fn rejects_bad_expressions() {
        for bad in [
            "",
            "* * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "*/0 * * * *",
            "5-1 * * * *",
            "* * * * funday",
            "0 0 0 8 * * 1-5",
        ] {
            assert!(bad.parse::<Cron>().is_err(), "{bad:?} should be rejected");
        }
        let e = "0 8 * * 1-9".parse::<Cron>().unwrap_err().to_string();
        assert!(e.contains("day of week"), "{e}");
    }
}
