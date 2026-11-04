//! Request rates like `"60/min"` (connector rate limits, tooling spec §6.1).

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// `count` requests per `per`. Written `"<n>/sec"`, `"<n>/min"` or
/// `"<n>/hour"` (`s`, `m`, `h` and plurals accepted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Rate {
    pub count: u32,
    pub per: Duration,
}

/// The most a rate may allow, so a typo can't make it meaningless.
const MAX_COUNT: u32 = 100_000;

impl Rate {
    pub const fn per_minute(count: u32) -> Self {
        Self {
            count,
            per: Duration::from_secs(60),
        }
    }
}

impl FromStr for Rate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("rate {s:?} must look like \"60/min\" (per sec, min or hour)");
        let (n, unit) = s.trim().split_once('/').ok_or_else(bad)?;
        let count: u32 = n.trim().parse().map_err(|_| bad())?;
        let secs = match unit.trim() {
            "s" | "sec" | "secs" | "second" | "seconds" => 1,
            "m" | "min" | "mins" | "minute" | "minutes" => 60,
            "h" | "hour" | "hours" => 3600,
            _ => return Err(bad()),
        };
        if count == 0 || count > MAX_COUNT {
            return Err(format!("rate {s:?}: the count must be 1 to {MAX_COUNT}"));
        }
        Ok(Self {
            count,
            per: Duration::from_secs(secs),
        })
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let unit = match self.per.as_secs() {
            1 => "sec",
            3600 => "hour",
            _ => "min",
        };
        write!(f, "{}/{unit}", self.count)
    }
}

impl TryFrom<String> for Rate {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        s.parse()
    }
}

impl From<Rate> for String {
    fn from(r: Rate) -> Self {
        r.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints() {
        for (s, shown) in [
            ("60/min", "60/min"),
            ("5/s", "5/sec"),
            (" 1000 / hour ", "1000/hour"),
            ("2/minutes", "2/min"),
        ] {
            assert_eq!(s.parse::<Rate>().unwrap().to_string(), shown);
        }
        for bad in ["60", "0/min", "x/min", "60/day", "100001/s", "-1/min"] {
            assert!(bad.parse::<Rate>().is_err(), "{bad}");
        }
    }
}
