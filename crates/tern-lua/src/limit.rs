//! Per-connector request limits (tooling spec §6.1): a token bucket per
//! connector, shared by every run, routine and script that calls it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tern_core::config::Rate;

/// The longest a server's `Retry-After` can pause a connector.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(3600);

struct Bucket {
    rate: Rate,
    tokens: f64,
    last: Instant,
    /// The API asked us to wait (`Retry-After` on a 429 or 503).
    blocked_until: Option<Instant>,
}

#[derive(Default)]
pub struct Limiter(Mutex<HashMap<String, Bucket>>);

impl Limiter {
    /// Take one request for `connector`, or say how long to wait. A full
    /// bucket holds `rate.count` requests, so a burst up to the limit goes
    /// through at once and then requests refill evenly.
    pub fn take(&self, connector: &str, rate: Rate) -> Result<(), String> {
        self.take_at(connector, rate, Instant::now())
    }

    fn take_at(&self, connector: &str, rate: Rate, now: Instant) -> Result<(), String> {
        let Ok(mut map) = self.0.lock() else {
            return Ok(());
        };
        let capacity = f64::from(rate.count);
        let b = map.entry(connector.to_owned()).or_insert(Bucket {
            rate,
            tokens: capacity,
            last: now,
            blocked_until: None,
        });
        if b.rate != rate {
            // Activated again with another rate: keep what was used.
            b.rate = rate;
            b.tokens = b.tokens.min(capacity);
        }
        let per_token = rate.per.as_secs_f64() / capacity;
        b.tokens = (b.tokens + now.saturating_duration_since(b.last).as_secs_f64() / per_token)
            .min(capacity);
        b.last = now;
        if let Some(until) = b.blocked_until {
            if until > now {
                return Err(format!(
                    "rate limited: the API asked {connector} to wait (Retry-After); try again in {}",
                    secs(until - now)
                ));
            }
            b.blocked_until = None;
        }
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            return Ok(());
        }
        let wait = Duration::from_secs_f64((1.0 - b.tokens) * per_token);
        Err(format!(
            "rate limited: connector {connector} may send {rate} requests and has used them; try again in {}. Nothing was sent.",
            secs(wait)
        ))
    }

    /// The server answered 429 or 503 with `Retry-After: <seconds>`.
    pub fn retry_after(&self, connector: &str, wait: Duration) {
        if let Ok(mut map) = self.0.lock()
            && let Some(b) = map.get_mut(connector)
        {
            b.blocked_until = Some(Instant::now() + wait.min(MAX_RETRY_AFTER));
        }
    }
}

fn secs(d: Duration) -> String {
    format!("{:.1}s", d.as_secs_f64().max(0.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bursts_to_the_limit_then_refills_evenly() {
        let l = Limiter::default();
        let rate: Rate = "3/min".parse().unwrap();
        let t0 = Instant::now();
        for _ in 0..3 {
            l.take_at("x", rate, t0).unwrap();
        }
        let err = l.take_at("x", rate, t0).unwrap_err();
        assert!(err.contains("connector x may send 3/min"), "{err}");
        assert!(err.contains("try again in 20.0s"), "{err}");
        // Another connector has its own bucket.
        l.take_at("y", rate, t0).unwrap();
        // One request comes back every 20 s.
        l.take_at("x", rate, t0 + Duration::from_secs(21)).unwrap();
        assert!(l.take_at("x", rate, t0 + Duration::from_secs(22)).is_err());
    }

    #[test]
    fn retry_after_pauses_the_connector() {
        let l = Limiter::default();
        let rate: Rate = "100/sec".parse().unwrap();
        l.take("x", rate).unwrap();
        l.retry_after("x", Duration::from_secs(30));
        let err = l.take("x", rate).unwrap_err();
        assert!(err.contains("asked x to wait (Retry-After)"), "{err}");
    }
}
