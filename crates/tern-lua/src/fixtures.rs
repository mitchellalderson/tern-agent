//! Recorded test fixtures (tooling spec §6.6, T4c): a connector's tool calls
//! run once against the live API, their HTTP exchanges saved sanitized
//! beside the connector (`<name>.fixtures.json` next to `<name>.luau`), and
//! replayed offline by later tests: no network, no secrets, no approval.
//!
//! Nothing secret is saved: credentials tern injects never reach the tape
//! (requests are recorded before injection), every known secret value is
//! replaced, sensitive headers are dropped, and JSON fields and query
//! parameters with secret-like names have their string values redacted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u32 = 1;
/// `<name>.fixtures.json` beside `<name>.luau`.
pub const SUFFIX: &str = "fixtures.json";
pub const REDACTED: &str = "[REDACTED]";
/// Most cases kept per connector.
const MAX_CASES: usize = 200;
/// Largest response body saved; longer ones are cut.
const MAX_BODY: usize = 256 * 1024;

/// Where a connector's fixtures live: beside its file.
pub fn path_for(connector: &Path) -> PathBuf {
    let stem = connector
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    connector.with_file_name(format!("{stem}.{SUFFIX}"))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub method: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

impl Request {
    /// Whether a replayed request is this recorded one: method, URL (query
    /// order aside) and body (JSON compared as JSON). Headers aren't
    /// compared.
    fn matches(&self, other: &Request) -> bool {
        self.method.eq_ignore_ascii_case(&other.method)
            && self.url == other.url
            && match (&self.body, &other.body) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a == b
                        || matches!(
                            (serde_json::from_str::<Value>(a), serde_json::from_str::<Value>(b)),
                            (Ok(x), Ok(y)) if x == y
                        )
                }
                _ => false,
            }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub status: u16,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exchange {
    pub request: Request,
    pub response: Response,
}

/// One recorded tool call: what it was asked, what it sent and got, and
/// what it returned (as replay produces it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    pub tool: String,
    #[serde(default)]
    pub args: Value,
    /// Unix ms.
    #[serde(default)]
    pub recorded_at: i64,
    pub result: String,
    #[serde(default)]
    pub exchanges: Vec<Exchange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fixtures {
    pub version: u32,
    pub connector: String,
    #[serde(default)]
    pub cases: Vec<Case>,
}

impl Fixtures {
    pub fn new(connector: &str) -> Self {
        Self {
            version: VERSION,
            connector: connector.to_owned(),
            cases: Vec::new(),
        }
    }

    /// The fixtures at `path`, if there are any.
    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        let f: Self = serde_json::from_str(&text)
            .map_err(|e| format!("{} isn't a fixtures file: {e}", path.display()))?;
        if f.version != VERSION {
            return Err(format!(
                "{} is fixtures version {}; this tern reads version {VERSION}",
                path.display(),
                f.version
            ));
        }
        Ok(Some(f))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text + "\n")
            .and_then(|()| std::fs::rename(&tmp, path))
            .map_err(|e| format!("writing {}: {e}", path.display()))
    }

    /// The recorded call of `tool` with exactly these arguments.
    pub fn find(&self, tool: &str, args: &Value) -> Option<&Case> {
        self.cases
            .iter()
            .find(|c| c.tool == tool && same_args(&c.args, args))
    }

    /// Add a case, replacing one for the same tool and arguments.
    pub fn upsert(&mut self, case: Case) {
        self.cases
            .retain(|c| !(c.tool == case.tool && same_args(&c.args, &case.args)));
        self.cases.push(case);
        if self.cases.len() > MAX_CASES {
            let extra = self.cases.len() - MAX_CASES;
            self.cases.drain(..extra);
        }
    }
}

/// `{}` and a missing `args` are the same call.
fn same_args(a: &Value, b: &Value) -> bool {
    let empty = |v: &Value| v.is_null() || v.as_object().is_some_and(|o| o.is_empty());
    a == b || (empty(a) && empty(b))
}

/// Recording or replaying one tool call's HTTP exchanges. `ctx.http` checks
/// it before (replay) and after (record) the network.
pub struct Tape {
    replay: Option<Vec<Exchange>>,
    used: Mutex<Vec<bool>>,
    recorded: Mutex<Vec<Exchange>>,
}

impl Tape {
    pub fn record() -> Self {
        Self {
            replay: None,
            used: Mutex::new(Vec::new()),
            recorded: Mutex::new(Vec::new()),
        }
    }

    pub fn replay(exchanges: Vec<Exchange>) -> Self {
        let used = vec![false; exchanges.len()];
        Self {
            replay: Some(exchanges),
            used: Mutex::new(used),
            recorded: Mutex::new(Vec::new()),
        }
    }

    pub fn is_replay(&self) -> bool {
        self.replay.is_some()
    }

    /// The recorded response for a request, preferring one not yet served
    /// (the same request twice gets its responses in order).
    pub(crate) fn take(&self, req: &Request) -> Option<Response> {
        let list = self.replay.as_ref()?;
        let mut used = self.used.lock().ok()?;
        let hits: Vec<usize> = (0..list.len())
            .filter(|&i| list[i].request.matches(req))
            .collect();
        let i = hits
            .iter()
            .copied()
            .find(|&i| !used[i])
            .or_else(|| hits.last().copied())?;
        used[i] = true;
        Some(list[i].response.clone())
    }

    pub(crate) fn push(&self, request: Request, response: Response) {
        if let Ok(mut r) = self.recorded.lock() {
            r.push(Exchange { request, response });
        }
    }

    pub fn exchanges(&self) -> Vec<Exchange> {
        self.recorded.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

/// Run `tool` live, recording its exchanges, and return the case. Its
/// `result` is what replaying the recording produces (so later replays
/// compare like with like), or the live result if the replay differs in a
/// way that fails.
pub async fn record_case(
    vm: &crate::Vm,
    tool: &str,
    args: &Value,
    conn: Option<tern_auth::Resolved>,
) -> Result<(String, Case), String> {
    let tape = std::sync::Arc::new(Tape::record());
    let live = vm.call_taped(tool, args, conn, Some(tape.clone())).await?;
    let exchanges = tape.exchanges();
    let mut case = Case {
        tool: tool.to_owned(),
        args: args.clone(),
        recorded_at: tern_store::now_ms(),
        result: String::new(),
        exchanges,
    };
    case.result = match case.exchanges.is_empty() {
        true => vm.host.secrets.redact(&live),
        false => replay_case(vm, &case)
            .await
            .unwrap_or_else(|_| vm.host.secrets.redact(&live)),
    };
    Ok((live, case))
}

/// Run a recorded case offline: every request is answered from the tape,
/// and one that wasn't recorded fails.
pub async fn replay_case(vm: &crate::Vm, case: &Case) -> Result<String, String> {
    let tape = std::sync::Arc::new(Tape::replay(case.exchanges.clone()));
    vm.call_taped(&case.tool, &case.args, None, Some(tape))
        .await
}

/// How a replayed case came out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    /// It ran, but returned something other than what was recorded.
    Changed {
        got: String,
    },
    Failed(String),
}

pub async fn check_case(vm: &crate::Vm, case: &Case) -> Outcome {
    match replay_case(vm, case).await {
        Ok(got) if got == case.result => Outcome::Pass,
        Ok(got) => Outcome::Changed { got },
        Err(e) => Outcome::Failed(e),
    }
}

/// A key whose string values are credentials: `access_token`, `apiKey`,
/// `X-Api-Key`, `password`, `client_secret`, `Set-Cookie`. Pagination
/// tokens (`next_page_token`, `cursor`) aren't.
pub fn sensitive(key: &str) -> bool {
    let k: String = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    if ["page", "next", "cursor", "continuation", "sync"]
        .iter()
        .any(|p| k.contains(p))
    {
        return false;
    }
    [
        "token",
        "secret",
        "password",
        "passwd",
        "apikey",
        "authorization",
        "cookie",
        "session",
        "signature",
        "credential",
        "privatekey",
        "accesskey",
        "signingkey",
        "sigkey",
    ]
    .iter()
    .any(|p| k.contains(p))
        || matches!(k.as_str(), "key" | "auth" | "xauth")
}

/// Replace the string values of sensitive fields, at any depth.
pub fn redact_json(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if sensitive(k) && x.is_string() {
                    *x = Value::String(REDACTED.into());
                } else {
                    redact_json(x);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(redact_json),
        _ => {}
    }
}

/// A body as saved: secret values scrubbed, then (JSON) sensitive fields
/// redacted and re-encoded compactly.
pub(crate) fn clean_body(raw: &str, scrub: &dyn Fn(&str) -> String) -> String {
    let text = scrub(raw);
    match serde_json::from_str::<Value>(&text) {
        Ok(mut j) if j.is_object() || j.is_array() => {
            redact_json(&mut j);
            j.to_string()
        }
        _ => text,
    }
}

/// A URL as saved and matched: sensitive query values redacted, the query
/// sorted, secret values scrubbed.
pub(crate) fn clean_url(url: &reqwest::Url, scrub: &dyn Fn(&str) -> String) -> String {
    let mut u = url.clone();
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| {
            let v = if sensitive(&k) {
                REDACTED.to_owned()
            } else {
                v.into_owned()
            };
            (k.into_owned(), v)
        })
        .collect();
    pairs.sort();
    if pairs.is_empty() {
        u.set_query(None);
    } else {
        u.query_pairs_mut().clear().extend_pairs(pairs);
    }
    u.set_fragment(None);
    scrub(u.as_str())
}

/// Request headers worth keeping: anything not sensitive, scrubbed.
pub(crate) fn clean_request_headers(
    headers: &reqwest::header::HeaderMap,
    scrub: &dyn Fn(&str) -> String,
) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter(|(k, _)| !sensitive(k.as_str()))
        .filter_map(|(k, v)| Some((k.as_str().to_owned(), scrub(v.to_str().ok()?))))
        .collect()
}

/// Response headers a connector may reasonably read; everything else
/// (cookies, auth challenges, tracing ids) is dropped.
pub(crate) fn keep_response_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "content-type"
            | "link"
            | "location"
            | "etag"
            | "last-modified"
            | "retry-after"
            | "x-total-count"
            | "x-next-page"
            | "x-page"
            | "x-per-page"
            | "cache-control"
    ) || n.starts_with("x-ratelimit")
        || n.starts_with("x-rate-limit")
        || n.starts_with("ratelimit")
}

/// A response body as saved, cut to a size worth keeping in a repo.
pub(crate) fn clip(body: String) -> String {
    if body.len() <= MAX_BODY {
        return body;
    }
    let mut end = MAX_BODY;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    body[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sensitive_names() {
        for k in [
            "access_token",
            "refresh_token",
            "apiKey",
            "X-Api-Key",
            "password",
            "client_secret",
            "Set-Cookie",
            "Authorization",
            "key",
            "session_id",
            "sig_key",
        ] {
            assert!(sensitive(k), "{k}");
        }
        for k in [
            "next_page_token",
            "cursor",
            "name",
            "status",
            "pageToken",
            "parcel_id",
            "monkey_business",
            "author",
        ] {
            assert!(!sensitive(k), "{k}");
        }
    }

    #[test]
    fn json_is_redacted_but_numbers_kept() {
        let mut v = json!({
            "user": { "name": "a", "access_token": "tok", "tokens": 12 },
            "items": [{ "api_key": "k" }],
            "next_page_token": "abc"
        });
        redact_json(&mut v);
        assert_eq!(v["user"]["access_token"], REDACTED);
        assert_eq!(v["user"]["tokens"], 12);
        assert_eq!(v["items"][0]["api_key"], REDACTED);
        assert_eq!(v["next_page_token"], "abc");
        assert_eq!(v["user"]["name"], "a");
    }

    #[test]
    fn urls_sorted_and_redacted() {
        let u = reqwest::Url::parse("https://x.com/a?b=2&api_key=s3cret&a=1#f").unwrap();
        let out = clean_url(&u, &|s: &str| s.replace("zzz", "[REDACTED]"));
        assert_eq!(out, "https://x.com/a?a=1&api_key=%5BREDACTED%5D&b=2");
    }

    #[test]
    fn upsert_replaces_same_call() {
        let mut f = Fixtures::new("x");
        let case = |r: &str| Case {
            tool: "t".into(),
            args: json!({}),
            recorded_at: 0,
            result: r.into(),
            exchanges: vec![],
        };
        f.upsert(case("one"));
        f.upsert(Case {
            args: Value::Null,
            ..case("two")
        });
        assert_eq!(f.cases.len(), 1);
        assert_eq!(f.find("t", &json!({})).unwrap().result, "two");
    }
}
