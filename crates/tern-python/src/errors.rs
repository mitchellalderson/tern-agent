//! Errors the model can act on (docs/python-tool-spec.md §6.2): Monty's
//! traceback, plus one line on what to write instead when the cause is a
//! missing feature, module or limit.

use std::collections::BTreeMap;

use monty_types::{ExcType, MontyException, OsFunctionCall};
use tern_core::types::ToolSpec;

/// Message prefix for the host-call cap.
pub const HOST_CALL_LIMIT: &str = "stopped: too many tool calls in one script; the limit is";

pub const NO_ASYNC: &str = "tool calls in tern's python are synchronous: call them directly, \
without await or asyncio.gather";

/// Modules people reach for that only real CPython has.
const PACKAGES: &[&str] = &[
    "pandas",
    "numpy",
    "scipy",
    "matplotlib",
    "requests",
    "httpx",
    "bs4",
    "yaml",
    "dateutil",
    "sklearn",
];

const AVAILABLE: &str = "json, re, csv, math, datetime, collections, itertools, functools, \
decimal, random, hashlib, base64, bisect, tomllib, pathlib, copy, fnmatch, unicodedata, typing";

/// One-line hints keyed on Monty's messages (docs/python-tool-spec.md
/// §6.2). The tests run each case through Monty, so a release that adds the
/// feature (and changes the message) shows up.
const FEATURE_HINTS: &[(&str, &str)] = &[
    (
        "class inheritance",
        "classes can't inherit in tern's python: use functions and dicts, or a @dataclass without a base class",
    ),
    (
        "yield",
        "generators aren't supported: build and return a list instead",
    ),
    (
        "del",
        "`del` isn't supported: use d.pop(key) for dicts, or build a new list or dict without the item",
    ),
    (
        "decorator",
        "method decorators (@property, @classmethod, @staticmethod) aren't supported: use plain methods or functions",
    ),
];

const MODULE_HINTS: &[(&str, &str)] = &[
    (
        "statistics",
        "use math and sorted(): mean = sum(xs) / len(xs); median = sorted(xs)[len(xs) // 2]",
    ),
    ("enum", "use plain constants or a dict of names to values"),
    (
        "string",
        "use str methods and literals, e.g. 'abc'.upper() or 'abcdefghijklmnopqrstuvwxyz'",
    ),
    (
        "uuid",
        "there is no uuid; use hashlib on the data for a stable id",
    ),
    ("operator", "use a lambda, e.g. key=lambda x: x['price']"),
    (
        "io",
        "there is no io; read files with open() under /workspace",
    ),
    (
        "contextlib",
        "there is no contextlib; write the try/finally out",
    ),
];

/// The error text for the model: the traceback, plus a hint when there is one.
pub fn explain(e: &MontyException, callable: &BTreeMap<String, ToolSpec>) -> String {
    let text = e.to_string();
    match hint(e, callable) {
        Some(h) => format!("{text}\nhint: {h}"),
        None => text,
    }
}

fn hint(e: &MontyException, callable: &BTreeMap<String, ToolSpec>) -> Option<String> {
    let msg = e.message().unwrap_or("");
    match e.exc_type() {
        ExcType::NotImplementedError => FEATURE_HINTS
            .iter()
            .find(|(needle, _)| msg.contains(needle))
            .map(|(_, h)| (*h).to_owned()),
        ExcType::ModuleNotFoundError => {
            let module = msg.split('\'').nth(1).unwrap_or("");
            let top = module.split('.').next().unwrap_or(module);
            if PACKAGES.contains(&top) {
                return Some(format!(
                    "'{top}' isn't available: tern's python has no packages. Do the computation with the \
                     standard modules ({AVAILABLE}), or use shell with python3 (it asks the user)"
                ));
            }
            let specific = MODULE_HINTS
                .iter()
                .find(|(m, _)| *m == top)
                .map(|(_, h)| format!("{h}. "))
                .unwrap_or_default();
            Some(format!("{specific}Available modules: {AVAILABLE}"))
        }
        ExcType::TimeoutError => Some(
            "the script ran out of interpreter time: do less work per script or use a cheaper \
             algorithm"
                .into(),
        ),
        ExcType::MemoryError => {
            Some("the script hit its memory limit: keep less data in memory".into())
        }
        ExcType::RecursionError => Some("recursion is limited to 1000 frames: use a loop".into()),
        ExcType::NameError if !callable.is_empty() => {
            let name = msg.split('\'').nth(1).unwrap_or("");
            similar(name, callable).map(|s| format!("did you mean {s}?"))
        }
        _ if msg.starts_with(HOST_CALL_LIMIT) => {
            Some("batch fewer lookups per script, or summarize what you have so far".into())
        }
        _ => None,
    }
}

/// The NameError for a function the script can't call.
pub fn unknown_function(name: &str, callable: &BTreeMap<String, ToolSpec>) -> MontyException {
    let mut msg = format!("name '{name}' is not defined");
    if let Some(s) = similar(name, callable) {
        msg.push_str(&format!(". Did you mean {s}?"));
    } else if callable.is_empty() {
        msg.push_str(". No tools can be called from this script");
    }
    MontyException::new(ExcType::NameError, Some(msg))
}

/// A callable tool whose name ends with or contains `name` (models drop
/// the `lua__connector__` prefix).
fn similar<'a>(name: &str, callable: &'a BTreeMap<String, ToolSpec>) -> Option<&'a str> {
    if name.len() < 3 {
        return None;
    }
    let tail = name.rsplit("__").next().unwrap_or(name);
    let flat = name.replace("__", "_");
    callable
        .keys()
        .find(|k| {
            k.ends_with(&format!("__{tail}"))
                || k.contains(name)
                || k.replace("__", "_").ends_with(&flat)
        })
        .map(String::as_str)
}

pub fn os_call_name(call: &OsFunctionCall) -> &'static str {
    call.name()
}
