//! What the activation card shows: every test of a draft at its exact
//! version (the source-and-reach hash the approval is bound to), with the
//! input, each HTTP call, the output or error, and the logs. Kept beside
//! the draft and appended to as each test finishes. It informs the
//! approval; it is not part of it.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

const VERSION: u32 = 1;
/// Most records kept per draft (the oldest go first).
const MAX_RECORDS: usize = 60;
const MAX_OUTPUT: usize = 600;
const MAX_LOG: usize = 300;
pub const SUFFIX: &str = "evidence.json";

/// One thing a test did, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// An HTTP call. `refused` is set when the host or path is not one the
    /// connector declared (nothing was sent); `error` when it failed after.
    Http {
        method: String,
        url: String,
        status: Option<u16>,
        refused: Option<String>,
        error: Option<String>,
    },
    Log {
        level: String,
        msg: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRecord {
    /// The draft's hash (content and reach) this test ran against.
    pub hash: String,
    pub tool: String,
    pub args: Value,
    pub at: i64,
    /// Replayed from a recorded fixture: nothing went over the network.
    pub offline: bool,
    pub ok: bool,
    /// The output when `ok`, else the error.
    pub text: String,
    pub events: Vec<Event>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    version: u32,
    records: Vec<TestRecord>,
}

pub fn path_for(connector: &Path) -> std::path::PathBuf {
    let mut p = connector.to_path_buf();
    p.set_extension(SUFFIX);
    p
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Append a record (its text is clipped); a failure to save is returned.
pub fn append(path: &Path, mut rec: TestRecord) -> Result<(), String> {
    rec.text = clip(&rec.text, MAX_OUTPUT);
    for e in &mut rec.events {
        if let Event::Log { msg, .. } = e {
            *msg = clip(msg, MAX_LOG);
        }
    }
    let mut f = load_file(path);
    f.version = VERSION;
    f.records.push(rec);
    let over = f.records.len().saturating_sub(MAX_RECORDS);
    f.records.drain(..over);
    let json = serde_json::to_string_pretty(&f).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("saving test evidence: {e}"))
}

fn load_file(path: &Path) -> File {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// The tests that ran against exactly `hash`, oldest first.
pub fn for_hash(path: &Path, hash: &str) -> Vec<TestRecord> {
    load_file(path)
        .records
        .into_iter()
        .filter(|r| r.hash == hash)
        .collect()
}

/// The evidence as text for the approval card: each test, then the tools
/// no test of this version touched.
pub fn render(records: &[TestRecord], tools: &[String]) -> String {
    let mut out = String::new();
    if records.is_empty() {
        out.push_str("Tests of this version: none.");
    } else {
        out.push_str(&format!("Tests of this version: {}.", records.len()));
    }
    for (i, r) in records.iter().enumerate() {
        out.push_str(&format!(
            "\n{}. {} {}({}){}",
            i + 1,
            if r.ok { "✓" } else { "✗" },
            r.tool,
            clip(&r.args.to_string(), 160),
            if r.offline { " [offline replay]" } else { "" },
        ));
        for e in &r.events {
            match e {
                Event::Http {
                    method,
                    url,
                    status,
                    refused,
                    error,
                } => {
                    let outcome = match (refused, error, status) {
                        (Some(_), _, _) => "Refused".to_owned(),
                        (_, Some(e), _) => format!("failed: {}", clip(e, 80)),
                        (_, _, Some(s)) => s.to_string(),
                        _ => "no response".to_owned(),
                    };
                    out.push_str(&format!("\n     {method} {url} → {outcome}"));
                }
                Event::Log { level, msg } => out.push_str(&format!("\n     log {level}: {msg}")),
            }
        }
        out.push_str(&format!(
            "\n     {}: {}",
            if r.ok { "output" } else { "error" },
            r.text.replace('\n', " ")
        ));
    }
    let untested: Vec<&String> = tools
        .iter()
        .filter(|t| !records.iter().any(|r| &r.tool == *t))
        .collect();
    if !untested.is_empty() {
        out.push_str(&format!(
            "\nNot tested: {}.",
            untested
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(hash: &str, tool: &str, ok: bool) -> TestRecord {
        TestRecord {
            hash: hash.into(),
            tool: tool.into(),
            args: serde_json::json!({"q": "x"}),
            at: 1,
            offline: false,
            ok,
            text: "done".into(),
            events: vec![
                Event::Http {
                    method: "GET".into(),
                    url: "https://api.x.com/v1".into(),
                    status: Some(200),
                    refused: None,
                    error: None,
                },
                Event::Http {
                    method: "GET".into(),
                    url: "https://evil.com/".into(),
                    status: None,
                    refused: Some("not declared".into()),
                    error: None,
                },
            ],
        }
    }

    #[test]
    fn only_this_versions_tests_render_and_untested_tools_are_named() {
        let dir = tempfile::tempdir().unwrap();
        let p = path_for(&dir.path().join("x.luau"));
        append(&p, rec("old", "a", true)).unwrap();
        append(&p, rec("new", "a", true)).unwrap();
        let got = for_hash(&p, "new");
        assert_eq!(got.len(), 1);
        let text = render(&got, &["a".into(), "b".into()]);
        assert!(text.contains("GET https://api.x.com/v1 → 200"), "{text}");
        assert!(text.contains("https://evil.com/ → Refused"), "{text}");
        assert!(text.contains("Not tested: b."), "{text}");
        assert!(render(&[], &["a".into()]).contains("none"));
    }
}
