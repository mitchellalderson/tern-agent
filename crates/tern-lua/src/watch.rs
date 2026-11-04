//! Watches and gates (tooling spec §3.2, §3.3; T1b): Luau that decides
//! whether a routine runs, so a check with nothing to report costs one
//! Luau call and no model call.
//!
//! A connector declares watches next to its tools:
//!
//! ```lua
//! watches = {
//!   review_requests = {
//!     description = "PRs where my review is requested",
//!     default_every = "5m",
//!     fn = function(ctx, cursor) return { events = { ... }, cursor = ... } end,
//!   },
//! }
//! ```
//!
//! Nothing here changes how connectors load. A watch or gate runs in a VM
//! of its own, loaded from a small wrapper around the connector's source
//! that exposes each watch (or the gate) as a `read` tool, through the same
//! [`Vm`] API tools use: the connector's declared hosts and secrets, its
//! time and memory limits, and GET-only HTTP (unless a watch sets
//! `read_via_post`). Only active connectors the user approved are used.

use std::path::Path;

use serde_json::{Value, json};

use crate::{EXTENSION, Host, Vm, load_active, valid_ident};

/// Largest event payload kept, as JSON text; longer ones are cut.
pub const MAX_PAYLOAD: usize = 16 * 1024;
/// Most events one poll may return; more is an error in the watch.
pub const MAX_EVENTS: usize = 1000;

/// A watch a connector declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchInfo {
    pub name: String,
    pub description: String,
    /// `5m`, `1h`, `90s`: how often it suggests polling.
    pub default_every: Option<String>,
}

/// One event from a poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The dedup key: the event's `id`, else a hash of the event.
    pub key: String,
    /// The event as JSON text, cut at [`MAX_PAYLOAD`].
    pub payload: String,
    /// The watch said `origin = "self"`: tern caused it.
    pub from_self: bool,
}

/// What one poll found.
#[derive(Debug, Clone, PartialEq)]
pub struct Poll {
    pub events: Vec<Event>,
    /// Opaque; handed back on the next poll. `null` when the watch keeps none.
    pub cursor: Value,
}

/// The source of active connector `name`, checked against what the user
/// approved, and unchanged while it was checked.
pub(crate) fn active_source(host: &Host, dir: &Path, name: &str) -> Result<String, String> {
    if !valid_ident(name) {
        return Err(format!("{name:?} is not a connector name"));
    }
    let path = dir.join(format!("{name}.{EXTENSION}"));
    let read = || {
        std::fs::read_to_string(&path).map_err(|e| format!("connector {name} is not active ({e})"))
    };
    let before = read()?;
    load_active(host, &path).map_err(|e| format!("connector {name}: {e}"))?;
    if read()? != before {
        return Err(format!("connector {name} changed while loading; try again"));
    }
    Ok(before)
}

/// The connector's table, evaluated in a function so its `return` is ours.
pub(crate) fn connector_prelude(source: &str) -> String {
    format!("local __c = (function() {source}\nend)()\n")
}

const WATCH_TOOLS: &str = r#"
if type(__c) ~= "table" then error("the file must `return` a table") end
local __ws = type(__c.watches) == "table" and __c.watches or {}
local __tools = {
  meta = { description = "watches", effect = "read", run = function()
    local out = {}
    for name, w in pairs(__ws) do
      if type(w) == "table" then
        table.insert(out, { name = name, description = w.description, default_every = w.default_every,
                            runs = type(w.fn) == "function" })
      end
    end
    return { watches = out }
  end },
}
for name, w in pairs(__ws) do
  if type(name) == "string" and #name <= 30 and string.match(name, "^[a-z][a-z0-9_]*$")
     and type(w) == "table" and type(w.fn) == "function" then
    __tools["w_" .. name] = {
      description = "watch " .. name, effect = "read", read_via_post = w.read_via_post == true,
      run = function(ctx, args)
        local r = w.fn(ctx, args.cursor)
        if type(r) ~= "table" then error("watch " .. name .. " must return { events = {...}, cursor = ... }") end
        return { events = r.events or {}, cursor = r.cursor }
      end,
    }
  end
end
return { name = __c.name, description = __c.description, capabilities = __c.capabilities, tools = __tools }
"#;

fn watch_vm(host: &Host, dir: &Path, connector: &str) -> Result<Vm, String> {
    let source = active_source(host, dir, connector)?;
    let wrapped = format!("{}{WATCH_TOOLS}", connector_prelude(&source));
    Vm::load(host, &format!("{connector}.{EXTENSION}"), &wrapped).map_err(|e| e.to_string())
}

pub(crate) fn parse_json(out: &str) -> Result<Value, String> {
    serde_json::from_str(out).map_err(|e| format!("unexpected result {out:?}: {e}"))
}

/// The watches active connector `connector` declares.
pub async fn watches(host: &Host, dir: &Path, connector: &str) -> Result<Vec<WatchInfo>, String> {
    let vm = watch_vm(host, dir, connector)?;
    let out = parse_json(&vm.call("meta", &json!({})).await?)?;
    let mut list = Vec::new();
    for w in out["watches"].as_array().into_iter().flatten() {
        let name = w["name"].as_str().unwrap_or_default().to_owned();
        if !valid_ident(&name) || name.len() > 30 {
            return Err(format!(
                "watch name {name:?} must match [a-z][a-z0-9_]* (max 30 chars)"
            ));
        }
        if w["runs"] != true {
            return Err(format!("watches.{name}: `fn` must be a function"));
        }
        list.push(WatchInfo {
            description: w["description"].as_str().unwrap_or_default().to_owned(),
            default_every: w["default_every"].as_str().map(str::to_owned),
            name,
        });
    }
    list.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(list)
}

/// Poll `connector`'s watch `watch` with `cursor` (`null` on the first poll).
pub async fn poll(
    host: &Host,
    dir: &Path,
    connector: &str,
    watch: &str,
    cursor: &Value,
) -> Result<Poll, String> {
    if !valid_ident(watch) || watch.len() > 30 {
        return Err(format!("{watch:?} is not a watch name"));
    }
    let vm = watch_vm(host, dir, connector)?;
    let tool = format!("w_{watch}");
    if !vm.spec.tools.iter().any(|t| t.name == tool) {
        return Err(format!("connector {connector} has no watch {watch:?}"));
    }
    let out = parse_json(&vm.call(&tool, &json!({ "cursor": cursor })).await?)?;
    let events = match &out["events"] {
        Value::Array(a) => a.clone(),
        // An empty Lua table comes back as an object.
        Value::Object(o) if o.is_empty() => Vec::new(),
        _ => return Err("`events` must be a list".into()),
    };
    if events.len() > MAX_EVENTS {
        return Err(format!(
            "{} events in one poll; return at most {MAX_EVENTS} and keep the rest for the next poll with the cursor",
            events.len()
        ));
    }
    Ok(Poll {
        events: events.into_iter().map(event).collect(),
        cursor: out["cursor"].clone(),
    })
}

fn event(v: Value) -> Event {
    let key = match &v["id"] {
        Value::String(s) if !s.is_empty() => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => format!(
            "h:{}",
            &blake3::hash(v.to_string().as_bytes()).to_hex()[..32]
        ),
    };
    let from_self = v["origin"] == "self";
    let mut payload = v.to_string();
    if payload.len() > MAX_PAYLOAD {
        let mut cut = MAX_PAYLOAD;
        while !payload.is_char_boundary(cut) {
            cut -= 1;
        }
        payload.truncate(cut);
        payload.push_str("…[truncated]");
    }
    Event {
        key,
        payload,
        from_self,
    }
}

/// A gate's body: a statement block with `return`, or an expression.
fn gate_body(code: &str) -> String {
    let has_return = code
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|w| w == "return");
    if has_return {
        code.to_owned()
    } else {
        format!("return ({code})")
    }
}

fn gate_source(connector_source: Option<&str>, code: &str) -> String {
    gate_source_with(
        connector_source,
        code,
        "routine",
        "return {{ pass = __r and true or false }}",
    )
}

/// A webhook gate: `routine` and `event` in scope. `false`/`nil` drops the
/// event, `true` keeps it, anything else replaces it.
fn webhook_gate_source(connector_source: Option<&str>, code: &str) -> String {
    gate_source_with(
        connector_source,
        code,
        "args",
        "if __r == true then return {{ keep = true }} end
    if not __r then return {{ keep = false }} end
    return {{ keep = true, event = __r }}",
    )
}

fn gate_source_with(connector_source: Option<&str>, code: &str, arg: &str, ret: &str) -> String {
    let prelude = match connector_source {
        Some(src) => connector_prelude(src),
        None => "local __c = { name = \"gate\", description = \"routine gate\" }\n".to_owned(),
    };
    let unpack = if arg == "args" {
        "local routine, event = args.routine, args.event\n"
    } else {
        ""
    };
    format!(
        "{prelude}if type(__c) ~= \"table\" then error(\"the file must `return` a table\") end
local connector = __c
return {{ name = __c.name, description = __c.description or \"gate\", capabilities = __c.capabilities,
  tools = {{ gate = {{ description = \"routine gate\", effect = \"read\", run = function(ctx, {arg})
    {unpack}local __r = (function()
{body}
    end)()
    {ret}
  end }} }} }}
",
        body = gate_body(code),
        ret = ret.replace("{{", "{").replace("}}", "}"),
    )
}

/// Check that a gate compiles (with `connector`'s capabilities, if any),
/// without running it.
pub fn check_gate(
    host: &Host,
    dir: &Path,
    connector: Option<&str>,
    code: &str,
) -> Result<(), String> {
    let source = connector.map(|c| active_source(host, dir, c)).transpose()?;
    Vm::load(host, "gate.luau", &gate_source(source.as_deref(), code))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Run a gate: true starts the run, false skips it. `connector` lends its
/// declared hosts and secrets (GET only) and its tools, as `connector`;
/// without one the gate has no network. `routine` is its argument.
pub async fn gate(
    host: &Host,
    dir: &Path,
    connector: Option<&str>,
    code: &str,
    routine: &Value,
) -> Result<bool, String> {
    let source = connector.map(|c| active_source(host, dir, c)).transpose()?;
    let vm = Vm::load(host, "gate.luau", &gate_source(source.as_deref(), code))
        .map_err(|e| e.to_string())?;
    let out = parse_json(&vm.call("gate", routine).await?)?;
    Ok(out["pass"] == true)
}

/// Run a webhook routine's gate on `event` (T1c): `None` drops it, else
/// the event to run with (the same one, or what the gate returned instead).
/// Capabilities as for [`gate`].
pub async fn webhook_gate(
    host: &Host,
    dir: &Path,
    connector: Option<&str>,
    code: &str,
    routine: &Value,
    event: &Value,
) -> Result<Option<Value>, String> {
    let source = connector.map(|c| active_source(host, dir, c)).transpose()?;
    let vm = Vm::load(
        host,
        "gate.luau",
        &webhook_gate_source(source.as_deref(), code),
    )
    .map_err(|e| e.to_string())?;
    let out = parse_json(
        &vm.call("gate", &json!({ "routine": routine, "event": event }))
            .await?,
    )?;
    if out["keep"] != true {
        return Ok(None);
    }
    Ok(Some(match out.get("event") {
        Some(e) => e.clone(),
        None => event.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tern_core::Secrets;
    use tern_store::Store;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn host() -> (Host, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        // No file: no secrets.
        let secrets = tmp.path().join("secrets.toml");
        let mut host = Host::new(
            Store::open_in_memory().unwrap(),
            Secrets::load(&secrets).unwrap(),
        )
        .unwrap();
        host.timeout = Duration::from_millis(500);
        std::fs::create_dir_all(tmp.path().join("connectors")).unwrap();
        (host, tmp)
    }

    fn install(tmp: &tempfile::TempDir, name: &str, src: &str) -> std::path::PathBuf {
        let dir = tmp.path().join("connectors");
        std::fs::write(dir.join(format!("{name}.luau")), src).unwrap();
        dir
    }

    const GITHUB: &str = r#"
local function issues(ctx)
  return ctx.http.get(BASE .. "/search").json.items
end
return {
  name = "github",
  description = "GitHub",
  capabilities = { http = { "127.0.0.1" } },
  tools = { search = { description = "search", effect = "read", run = function(ctx) return issues(ctx) end } },
  watches = {
    reviews = {
      description = "PRs where my review is requested",
      default_every = "5m",
      fn = function(ctx, cursor)
        local seen, events = cursor or {}, {}
        for _, it in ipairs(issues(ctx)) do
          if not seen[tostring(it.id)] then
            table.insert(events, { id = it.id, title = it.title, origin = it.origin })
          end
          seen[tostring(it.id)] = true
        end
        return { events = events, cursor = seen }
      end,
    },
    broken = { description = "no fn" },
  },
}
"#;

    async fn github(items: Value) -> (Host, tempfile::TempDir, std::path::PathBuf, MockServer) {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": items })))
            .mount(&server)
            .await;
        let (host, tmp) = host();
        let src = format!("local BASE = \"{}\"\n{GITHUB}", server.uri());
        let dir = install(&tmp, "github", &src);
        (host, tmp, dir, server)
    }

    #[tokio::test]
    async fn a_watch_polls_with_its_cursor_and_returns_keyed_events() {
        let (host, _tmp, dir, _server) = github(json!([
            { "id": 7, "title": "Fix it" },
            { "id": 8, "title": "Mine", "origin": "self" }
        ]))
        .await;
        let err = watches(&host, &dir, "github").await.unwrap_err();
        assert!(err.contains("watches.broken"), "{err}");

        let first = poll(&host, &dir, "github", "reviews", &Value::Null)
            .await
            .unwrap();
        let keys: Vec<(&str, bool)> = first
            .events
            .iter()
            .map(|e| (e.key.as_str(), e.from_self))
            .collect();
        assert_eq!(keys, [("7", false), ("8", true)]);
        assert!(first.events[0].payload.contains("Fix it"));
        assert_eq!(first.cursor, json!({ "7": true, "8": true }));
        // The cursor comes back: nothing new.
        let again = poll(&host, &dir, "github", "reviews", &first.cursor)
            .await
            .unwrap();
        assert!(again.events.is_empty());
        assert_eq!(again.cursor, first.cursor);

        let err = poll(&host, &dir, "github", "nope", &Value::Null)
            .await
            .unwrap_err();
        assert!(err.contains("no watch"), "{err}");
        let err = poll(&host, &dir, "missing", "reviews", &Value::Null)
            .await
            .unwrap_err();
        assert!(err.contains("not active"), "{err}");
    }

    #[tokio::test]
    async fn watches_list_what_a_connector_declares() {
        let (host, tmp) = host();
        let dir = install(
            &tmp,
            "feed",
            r#"return { name = "feed", description = "f", tools = { x = { description = "x", effect = "read", run = function() end } },
              watches = { items = { description = "new items", default_every = "1h", fn = function() return { events = {} } end } } }"#,
        );
        let list = watches(&host, &dir, "feed").await.unwrap();
        assert_eq!(
            list,
            [WatchInfo {
                name: "items".into(),
                description: "new items".into(),
                default_every: Some("1h".into()),
            }]
        );
        let p = poll(&host, &dir, "feed", "items", &Value::Null)
            .await
            .unwrap();
        assert_eq!(
            p,
            Poll {
                events: Vec::new(),
                cursor: Value::Null
            }
        );
    }

    #[tokio::test]
    async fn a_watch_is_read_only_and_keeps_to_its_hosts() {
        let (host, tmp) = host();
        let dir = install(
            &tmp,
            "sneaky",
            r#"return { name = "sneaky", description = "s", capabilities = { http = { "127.0.0.1" } },
              tools = { x = { description = "x", effect = "read", run = function() end } },
              watches = {
                post = { fn = function(ctx) ctx.http.post("http://127.0.0.1:9/x", { body = "hi" }) return { events = {} } end },
                away = { fn = function(ctx) ctx.http.get("https://example.com/") return { events = {} } end },
              } }"#,
        );
        for w in ["post", "away"] {
            let err = poll(&host, &dir, "sneaky", w, &Value::Null)
                .await
                .unwrap_err();
            assert!(!err.is_empty(), "{w}");
        }
    }

    #[tokio::test]
    async fn events_without_an_id_are_keyed_by_content_and_long_ones_cut() {
        let e = event(json!({ "title": "a" }));
        assert_eq!(e.key, event(json!({ "title": "a" })).key);
        assert_ne!(e.key, event(json!({ "title": "b" })).key);
        assert!(e.key.starts_with("h:"));
        let long = event(json!({ "id": "x", "body": "é".repeat(MAX_PAYLOAD) }));
        assert!(long.payload.ends_with("…[truncated]") && long.payload.len() < MAX_PAYLOAD + 20);
    }

    #[tokio::test]
    async fn gates_pass_or_skip_and_can_use_a_connectors_tools() {
        let (host, _tmp, dir, _server) = github(json!([{ "id": 1, "title": "x" }])).await;
        let routine = json!({ "id": "sch_1" });
        let run = |c: Option<&'static str>, code: &'static str| {
            let (host, dir, routine) = (host.clone(), dir.clone(), routine.clone());
            async move { gate(&host, &dir, c, code, &routine).await }
        };
        assert!(run(None, "1 + 1 == 2").await.unwrap());
        assert!(!run(None, "false").await.unwrap());
        assert!(
            !run(
                None,
                "if routine.id == 'sch_1' then return nil end return true"
            )
            .await
            .unwrap()
        );
        assert!(
            run(Some("github"), "#connector.tools.search.run(ctx, {}) > 0")
                .await
                .unwrap()
        );
        // Without a connector there is no network at all.
        let err = run(None, "ctx.http.get('http://127.0.0.1:9/')")
            .await
            .unwrap_err();
        assert!(!err.is_empty());
        assert!(
            run(None, "error('boom')")
                .await
                .unwrap_err()
                .contains("boom")
        );
        assert!(check_gate(&host, &dir, None, "true and").is_err());
        assert!(check_gate(&host, &dir, Some("github"), "true").is_ok());
        assert!(check_gate(&host, &dir, Some("nope"), "true").is_err());
    }

    #[tokio::test]
    async fn webhook_gates_drop_keep_or_reshape_an_event() {
        let (host, tmp) = host();
        let dir = tmp.path().join("connectors");
        let routine = json!({ "id": "sch_1" });
        let event =
            json!({ "headers": { "x-github-event": "push" }, "body": { "ref": "main", "n": 3 } });
        let run = |code: &'static str| {
            let (host, dir, routine, event) =
                (host.clone(), dir.clone(), routine.clone(), event.clone());
            async move { webhook_gate(&host, &dir, None, code, &routine, &event).await }
        };
        assert_eq!(
            run("event.headers['x-github-event'] == 'pull_request'")
                .await
                .unwrap(),
            None
        );
        assert_eq!(run("nil").await.unwrap(), None);
        assert_eq!(run("true").await.unwrap(), Some(event.clone()));
        assert_eq!(
            run("return { ref = event.body.ref, routine = routine.id }")
                .await
                .unwrap(),
            Some(json!({ "ref": "main", "routine": "sch_1" }))
        );
        assert!(run("error('boom')").await.unwrap_err().contains("boom"));
    }
}
