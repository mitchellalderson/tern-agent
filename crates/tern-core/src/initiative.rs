//! Initiative (docs/personal-agent-tooling-spec.md §4.1, T1b): how loudly a
//! routine's result reaches the user. Every result is posted to its
//! channel; initiative decides the push, the message to the routine's chat
//! (`deliver_to`).
//!
//! | Level | Push |
//! |---|---|
//! | `silent` | none |
//! | `digest` | batched into one message at the next digest time |
//! | `notify` | if the gate model says it is worth interrupting; else `digest` |
//! | `interrupt` | always (quiet hours still hold it unless `urgent`) |

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Duration, Local};
use serde::{Deserialize, Serialize};

use crate::quiet::{at_local, minutes};

/// A routine's initiative level, quietest first.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Initiative {
    Silent,
    /// Routines start here (spec W8: conservative by default).
    #[default]
    Digest,
    Notify,
    Interrupt,
}

impl Initiative {
    pub const ALL: [Initiative; 4] = [
        Initiative::Silent,
        Initiative::Digest,
        Initiative::Notify,
        Initiative::Interrupt,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Digest => "digest",
            Self::Notify => "notify",
            Self::Interrupt => "interrupt",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|i| i.as_str().eq_ignore_ascii_case(s.trim()))
    }

    fn step(self, by: i8) -> Self {
        let i = (self as i8 + by).clamp(0, 3);
        Self::ALL[i as usize]
    }
}

impl fmt::Display for Initiative {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The global initiative dial (spec §4.1, Muse's): every routine one level
/// quieter or louder, or all silent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dial {
    /// Everything `silent`: results are posted, nothing is pushed.
    Vacation,
    Quieter,
    #[default]
    Normal,
    Louder,
}

impl Dial {
    pub const ALL: [Dial; 4] = [Dial::Vacation, Dial::Quieter, Dial::Normal, Dial::Louder];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vacation => "vacation",
            Self::Quieter => "quieter",
            Self::Normal => "normal",
            Self::Louder => "louder",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|d| d.as_str().eq_ignore_ascii_case(s.trim()))
    }

    /// The level a routine at `level` gets under this dial.
    pub fn apply(self, level: Initiative) -> Initiative {
        match self {
            Self::Vacation => Initiative::Silent,
            Self::Quieter => level.step(-1),
            Self::Normal => level,
            Self::Louder => level.step(1),
        }
    }
}

/// When digests go out: local `HH:MM` times, every day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<String>", into = "Vec<String>")]
pub struct DigestTimes(Vec<u32>);

impl Default for DigestTimes {
    fn default() -> Self {
        Self(vec![8 * 60 + 30, 17 * 60 + 30])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("digest time {0:?}: expected \"HH:MM\"")]
pub struct DigestTimeError(String);

impl TryFrom<Vec<String>> for DigestTimes {
    type Error = DigestTimeError;

    fn try_from(v: Vec<String>) -> Result<Self, Self::Error> {
        let mut out = v
            .iter()
            .map(|s| minutes(s).ok_or_else(|| DigestTimeError(s.clone())))
            .collect::<Result<Vec<_>, _>>()?;
        out.sort_unstable();
        out.dedup();
        Ok(Self(out))
    }
}

impl From<DigestTimes> for Vec<String> {
    fn from(d: DigestTimes) -> Self {
        d.0.iter()
            .map(|m| format!("{:02}:{:02}", m / 60, m % 60))
            .collect()
    }
}

impl FromStr for DigestTimes {
    type Err = DigestTimeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.split(',')
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .try_into()
    }
}

impl DigestTimes {
    /// The first digest time after `now`; with none set, a day from now.
    pub fn next_after(&self, now: DateTime<Local>) -> DateTime<Local> {
        let today = now.date_naive();
        for day in [today, today + Duration::days(1)] {
            for &m in &self.0 {
                let t = at_local(day, m);
                if t > now {
                    return t;
                }
            }
        }
        now + Duration::days(1)
    }
}

/// The gate model's instructions (spec §4.1): one cheap call that only
/// decides whether a `notify` result is worth a push now.
pub const GATE_SYSTEM: &str = "You decide whether a background task's result is worth interrupting the user for right \
now. Interrupt only for something time-sensitive or that needs them soon; routine updates can wait for the next \
digest. Answer with JSON only, no other text: {\"interrupt\": true or false, \"headline\": \"at most 80 characters \
saying what happened\"}.";

/// The gate model's question: what the routine is for, and what it found.
pub fn gate_prompt(purpose: &str, result: &str) -> String {
    let cut = |s: &str, n: usize| -> String {
        match s.char_indices().nth(n) {
            Some((i, _)) => format!("{}…", &s[..i]),
            None => s.to_owned(),
        }
    };
    format!(
        "Is this worth interrupting the user for?\n\nThe task:\n{}\n\nIts result:\n{}",
        cut(purpose.trim(), 1500),
        cut(result.trim(), 4000)
    )
}

/// What the gate model answered.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GateAnswer {
    pub interrupt: bool,
    #[serde(default)]
    pub headline: String,
}

/// The JSON object in the gate model's answer, tolerating text around it.
pub fn parse_gate_answer(text: &str) -> Option<GateAnswer> {
    let (start, end) = (text.find('{')?, text.rfind('}')?);
    let mut a: GateAnswer = serde_json::from_str(text.get(start..=end)?).ok()?;
    a.headline = a.headline.trim().chars().take(120).collect();
    Some(a)
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, TimeZone};

    use super::*;

    #[test]
    fn levels_parse_and_the_dial_moves_them() {
        assert_eq!(Initiative::parse(" Notify "), Some(Initiative::Notify));
        assert_eq!(Initiative::parse("loud"), None);
        assert_eq!(Initiative::default(), Initiative::Digest);
        use Initiative::*;
        for (dial, from, to) in [
            (Dial::Vacation, Interrupt, Silent),
            (Dial::Quieter, Interrupt, Notify),
            (Dial::Quieter, Silent, Silent),
            (Dial::Normal, Notify, Notify),
            (Dial::Louder, Digest, Notify),
            (Dial::Louder, Interrupt, Interrupt),
        ] {
            assert_eq!(dial.apply(from), to, "{dial:?} {from}");
        }
        assert_eq!(Dial::parse("VACATION"), Some(Dial::Vacation));
    }

    #[test]
    fn digest_times_come_round_each_day() {
        let at = |d: u32, h: u32, m: u32| {
            Local
                .from_local_datetime(
                    &NaiveDate::from_ymd_opt(2026, 11, d)
                        .unwrap()
                        .and_hms_opt(h, m, 0)
                        .unwrap(),
                )
                .unwrap()
        };
        let d = DigestTimes::default();
        assert_eq!(d.next_after(at(4, 7, 0)), at(4, 8, 30));
        assert_eq!(d.next_after(at(4, 8, 30)), at(4, 17, 30));
        assert_eq!(d.next_after(at(4, 20, 0)), at(5, 8, 30));
        let one: DigestTimes = "18:00, 07:15".parse().unwrap();
        assert_eq!(Vec::<String>::from(one.clone()), ["07:15", "18:00"]);
        assert_eq!(one.next_after(at(4, 12, 0)), at(4, 18, 0));
        assert!("25:00".parse::<DigestTimes>().is_err());
        let none: DigestTimes = "".parse().unwrap();
        assert_eq!(none.next_after(at(4, 12, 0)), at(5, 12, 0));
    }

    #[test]
    fn gate_answers_are_read_leniently() {
        assert_eq!(
            parse_gate_answer(
                "```json\n{\"interrupt\": true, \"headline\": \" Build broke \"}\n```"
            ),
            Some(GateAnswer {
                interrupt: true,
                headline: "Build broke".into()
            })
        );
        assert_eq!(
            parse_gate_answer("{\"interrupt\": false}").map(|a| a.interrupt),
            Some(false)
        );
        assert_eq!(parse_gate_answer("yes, interrupt"), None);
        let p = gate_prompt("Watch the build", &"x".repeat(5000));
        assert!(p.contains("Watch the build") && p.ends_with('…'));
    }
}
