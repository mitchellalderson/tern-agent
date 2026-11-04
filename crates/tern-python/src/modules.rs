//! Standard-library modules Monty 1.4.0 lacks, provided by tern: `statistics`
//! and `string`. Monty asks its host for any module it doesn't know
//! (`OsFunctionCall::Import`), and the host's answer becomes the module.
//! These are pure computation done in the interpreter's own process: a call
//! never reaches the agent loop, so it isn't journaled and costs no tool
//! call. They are deterministic, so a re-run gives the same values.

use monty_types::{CallArgs, ExcType, ExtFunctionResult, MontyException, MontyObject, MontyUuid};
use serde_json::Value;

use crate::convert;

const STATISTICS: &[&str] = &[
    "mean",
    "fmean",
    "median",
    "median_low",
    "median_high",
    "mode",
    "multimode",
    "stdev",
    "pstdev",
    "variance",
    "pvariance",
    "geometric_mean",
    "harmonic_mean",
    "quantiles",
];

const STRING_CONSTANTS: &[(&str, &str)] = &[
    ("ascii_lowercase", "abcdefghijklmnopqrstuvwxyz"),
    ("ascii_uppercase", "ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
    (
        "ascii_letters",
        "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ",
    ),
    ("digits", "0123456789"),
    ("hexdigits", "0123456789abcdefABCDEF"),
    ("octdigits", "01234567"),
    ("punctuation", "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"),
    ("whitespace", " \t\n\r\x0b\x0c"),
    (
        "printable",
        "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~ \t\n\r\x0b\x0c",
    ),
];

/// The module `name` as the sandbox's `import` should see it.
pub fn import(name: &str) -> Option<MontyObject> {
    match name {
        "statistics" => Some(MontyObject::module(
            "statistics",
            MontyUuid::from_u128(0x7465_726e_0001),
            STATISTICS.iter().map(|f| {
                (
                    MontyObject::string((*f).to_owned()),
                    MontyObject::function(format!("statistics.{f}"), None),
                )
            }),
        )),
        "string" => Some(MontyObject::module(
            "string",
            MontyUuid::from_u128(0x7465_726e_0002),
            STRING_CONSTANTS.iter().map(|(k, v)| {
                (
                    MontyObject::string((*k).to_owned()),
                    MontyObject::string((*v).to_owned()),
                )
            }),
        )),
        _ => None,
    }
}

/// Whether `function` (as Monty names it in a call) is one of ours.
pub fn provides(function: &str) -> bool {
    function
        .strip_prefix("statistics.")
        .is_some_and(|f| STATISTICS.contains(&f))
}

/// Run one of our functions.
pub fn call(function: &str, args: &CallArgs) -> ExtFunctionResult {
    let f = function.rsplit('.').next().unwrap_or(function);
    match stat(f, args) {
        Ok(v) => ExtFunctionResult::Return(v),
        Err((kind, msg)) => {
            ExtFunctionResult::Error(MontyException::new(kind, Some(format!("{f}: {msg}"))))
        }
    }
}

type Failure = (ExcType, String);

fn value_error<T>(msg: &str) -> Result<T, Failure> {
    Err((ExcType::ValueError, msg.to_owned()))
}

/// A number from the script: an int stays exact.
#[derive(Clone, Copy)]
enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    fn f(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Float(f) => f,
        }
    }

    fn object(self) -> MontyObject {
        match self {
            Num::Int(i) => MontyObject::int(i),
            Num::Float(f) => MontyObject::float(f),
        }
    }
}

/// The data argument (a list, tuple or other iterable the interpreter can
/// show as a sequence), without the arguments after it.
fn data(args: &CallArgs, f: &str) -> Result<Vec<Value>, Failure> {
    let all = convert::args_json(args);
    match all.first() {
        Some(Value::Array(items)) => Ok(items.clone()),
        Some(_) => Err((
            ExcType::TypeError,
            format!("{f}() needs a list of data points"),
        )),
        None => Err((
            ExcType::TypeError,
            format!("{f}() needs a list of data points"),
        )),
    }
}

fn numbers(args: &CallArgs, f: &str) -> Result<Vec<Num>, Failure> {
    data(args, f)?
        .iter()
        .map(|v| match v {
            Value::Number(n) => Ok(n
                .as_i64()
                .map_or_else(|| Num::Float(n.as_f64().unwrap_or(f64::NAN)), Num::Int)),
            Value::Bool(b) => Ok(Num::Int(i64::from(*b))),
            _ => Err((ExcType::TypeError, format!("{f}() needs numbers, got {v}"))),
        })
        .collect()
}

fn float(f: f64) -> MontyObject {
    MontyObject::float(f)
}

fn sorted(mut xs: Vec<Num>) -> Vec<Num> {
    xs.sort_by(|a, b| a.f().total_cmp(&b.f()));
    xs
}

fn variance(xs: &[Num], sample: bool) -> Result<f64, Failure> {
    let need = if sample { 2 } else { 1 };
    if xs.len() < need {
        return value_error(if sample {
            "variance requires at least two data points"
        } else {
            "pvariance requires at least one data point"
        });
    }
    let n = xs.len() as f64;
    let mean = xs.iter().map(|x| x.f()).sum::<f64>() / n;
    let ss: f64 = xs.iter().map(|x| (x.f() - mean).powi(2)).sum();
    Ok(ss / if sample { n - 1.0 } else { n })
}

fn stat(f: &str, args: &CallArgs) -> Result<MontyObject, Failure> {
    match f {
        "mode" | "multimode" => {
            let items = data(args, f)?;
            // Values are compared as JSON, in first-seen order.
            let mut counts: Vec<(Value, usize)> = Vec::new();
            for v in items {
                match counts.iter_mut().find(|(k, _)| *k == v) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((v, 1)),
                }
            }
            let best = counts.iter().map(|(_, n)| *n).max().unwrap_or(0);
            let winners = counts
                .into_iter()
                .filter(|(_, n)| *n == best)
                .map(|(v, _)| v);
            if f == "multimode" {
                return Ok(MontyObject::list(winners.map(|v| convert::to_monty(&v))));
            }
            match winners.into_iter().next() {
                Some(v) => Ok(convert::to_monty(&v)),
                None => value_error("no mode for empty data"),
            }
        }
        "mean" => {
            let xs = numbers(args, f)?;
            if xs.is_empty() {
                return value_error("mean requires at least one data point");
            }
            if xs.iter().all(|x| matches!(x, Num::Int(_))) {
                let sum: i128 = xs
                    .iter()
                    .map(|x| match x {
                        Num::Int(i) => i128::from(*i),
                        Num::Float(_) => 0,
                    })
                    .sum();
                let n = xs.len() as i128;
                if sum % n == 0
                    && let Ok(i) = i64::try_from(sum / n)
                {
                    return Ok(MontyObject::int(i));
                }
                return Ok(float(sum as f64 / n as f64));
            }
            Ok(float(
                xs.iter().map(|x| x.f()).sum::<f64>() / xs.len() as f64,
            ))
        }
        "fmean" => {
            let xs = numbers(args, f)?;
            if xs.is_empty() {
                return value_error("fmean requires at least one data point");
            }
            Ok(float(
                xs.iter().map(|x| x.f()).sum::<f64>() / xs.len() as f64,
            ))
        }
        "median" | "median_low" | "median_high" => {
            let xs = sorted(numbers(args, f)?);
            let n = xs.len();
            if n == 0 {
                return value_error("no median for empty data");
            }
            if n % 2 == 1 {
                return Ok(xs[n / 2].object());
            }
            let (lo, hi) = (xs[n / 2 - 1], xs[n / 2]);
            Ok(match f {
                "median_low" => lo.object(),
                "median_high" => hi.object(),
                _ => float((lo.f() + hi.f()) / 2.0),
            })
        }
        "variance" => Ok(float(variance(&numbers(args, f)?, true)?)),
        "pvariance" => Ok(float(variance(&numbers(args, f)?, false)?)),
        "stdev" => Ok(float(variance(&numbers(args, f)?, true)?.sqrt())),
        "pstdev" => Ok(float(variance(&numbers(args, f)?, false)?.sqrt())),
        "geometric_mean" => {
            let xs = numbers(args, f)?;
            if xs.is_empty() {
                return value_error("geometric mean requires a non-empty dataset");
            }
            if xs.iter().any(|x| x.f() <= 0.0) {
                return value_error(
                    "geometric mean requires a non-empty dataset containing positive numbers",
                );
            }
            let logs: f64 = xs.iter().map(|x| x.f().ln()).sum();
            Ok(float((logs / xs.len() as f64).exp()))
        }
        "harmonic_mean" => {
            let xs = numbers(args, f)?;
            if xs.is_empty() {
                return value_error("harmonic_mean requires at least one data point");
            }
            if xs.iter().any(|x| x.f() < 0.0) {
                return value_error("harmonic mean does not support negative values");
            }
            if xs.iter().any(|x| x.f() == 0.0) {
                return Ok(float(0.0));
            }
            let inv: f64 = xs.iter().map(|x| 1.0 / x.f()).sum();
            Ok(float(xs.len() as f64 / inv))
        }
        "quantiles" => {
            let xs = sorted(numbers(args, f)?);
            let n = convert::int_arg(args, 1, "n").unwrap_or(4);
            if n < 1 {
                return value_error("n must be at least 1");
            }
            if xs.len() < 2 {
                return value_error("must have at least two data points");
            }
            let ld = xs.len() as i64;
            let m = ld + 1;
            let out = (1..n).map(|i| {
                let j = (i * m / n).clamp(1, ld - 1);
                let delta = i * m - j * n;
                let (a, b) = (xs[(j - 1) as usize].f(), xs[j as usize].f());
                float((a * (n - delta) as f64 + b * delta as f64) / n as f64)
            });
            Ok(MontyObject::list(out))
        }
        other => Err((
            ExcType::AttributeError,
            format!("module 'statistics' has no attribute '{other}'"),
        )),
    }
}
