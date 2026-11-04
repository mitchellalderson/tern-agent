//! Gateway connectors (tooling spec §6.4; T4b-ii): a chat transport written
//! in Luau, so a new place to reach the user (ntfy, Slack, …) is a file,
//! not a Rust crate.
//!
//! A connector declares one next to its tools:
//!
//! ```lua
//! gateway = {
//!   kind = "ntfy",
//!   threads = false,
//!   send = function(ctx, to, text, opts) ... end,
//!   buttons = function(ctx, to, text, buttons, opts) ... end,   -- optional
//!   poll = function(ctx, cursor, settings)                       -- optional
//!     return { messages = { { id = "…", chat = "…", from = "…", text = "…" } }, cursor = … }
//!   end,
//! }
//! ```
//!
//! As with watches, nothing here changes how connectors load: each call
//! runs in a VM of its own, loaded from a wrapper around the connector's
//! source that exposes `send` and `buttons` as `write` tools and `poll` as
//! a `read` one (GET only unless the gateway sets `read_via_post`), with
//! the connector's declared hosts, credentials, rate and limits. Only
//! active connectors the user approved are used, and only those the user
//! enabled in `[gateway.connectors]` are run by the daemon.

use std::path::Path;

use serde_json::{Value, json};

use crate::watch::{active_source, connector_prelude, parse_json};
use crate::{EXTENSION, Host, Vm};

/// Most messages one poll may return.
pub const MAX_MESSAGES: usize = 100;
/// Longest inbound message kept; longer ones are cut.
pub const MAX_TEXT: usize = 16 * 1024;

/// What a connector's `gateway` table offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayInfo {
    /// `ntfy`, `slack`: shown to the user; the connector's name if unset.
    pub kind: String,
    /// Chats have threads (`<chat>/<thread>` refs).
    pub threads: bool,
    pub send: bool,
    /// Can show buttons; without them approvals ask for a numbered reply.
    pub buttons: bool,
    /// Can receive messages.
    pub poll: bool,
}

/// One button: what it says and the data a tap sends back as `action`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub label: String,
    pub data: String,
}

/// A message a poll found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inbound {
    /// The platform's id, for logs.
    pub id: Option<String>,
    /// Where it was written; replies go back there.
    pub chat: String,
    pub thread: Option<String>,
    /// Who wrote it, when the platform says. Without it the chat is the
    /// sender (ntfy topics have no users).
    pub from: Option<String>,
    /// A display name for the sender, for the taint note.
    pub from_name: Option<String>,
    pub text: String,
    /// A button tap: the button's `data`.
    pub action: Option<String>,
}

impl Inbound {
    /// Who pairing is about: the sender, else the chat.
    pub fn identity(&self) -> &str {
        self.from.as_deref().unwrap_or(&self.chat)
    }

    /// `<chat>` or, in a thread, `<chat>/<thread>`.
    pub fn chat_ref(&self) -> String {
        match &self.thread {
            Some(t) => format!("{}/{t}", self.chat),
            None => self.chat.clone(),
        }
    }
}

/// What one poll found.
#[derive(Debug, Clone, PartialEq)]
pub struct Poll {
    pub messages: Vec<Inbound>,
    /// Opaque; handed back on the next poll.
    pub cursor: Value,
}

const GATEWAY_TOOLS: &str = r#"
if type(__c) ~= "table" then error("the file must `return` a table") end
local __g = __c.gateway
if type(__g) ~= "table" then error("connector " .. tostring(__c.name) .. " declares no `gateway` table") end
local __tools = {
  meta = { description = "gateway", effect = "read", run = function()
    return { kind = __g.kind, threads = __g.threads == true, send = type(__g.send) == "function",
             buttons = type(__g.buttons) == "function", poll = type(__g.poll) == "function" }
  end },
}
if type(__g.send) == "function" then
  __tools.send = { description = "gateway send", effect = "write", run = function(ctx, a)
    return __g.send(ctx, a.to, a.text, a.opts or {})
  end }
end
if type(__g.buttons) == "function" then
  __tools.buttons = { description = "gateway buttons", effect = "write", run = function(ctx, a)
    return __g.buttons(ctx, a.to, a.text, a.buttons or {}, a.opts or {})
  end }
end
if type(__g.poll) == "function" then
  __tools.poll = { description = "gateway poll", effect = "read", read_via_post = __g.read_via_post == true,
    run = function(ctx, a)
      local r = __g.poll(ctx, a.cursor, a.settings or {})
      if type(r) ~= "table" then error("gateway.poll must return { messages = {...}, cursor = ... }") end
      return { messages = r.messages or {}, cursor = r.cursor }
    end }
end
return { name = __c.name, description = __c.description, capabilities = __c.capabilities, tools = __tools }
"#;

fn gateway_vm(host: &Host, dir: &Path, connector: &str) -> Result<Vm, String> {
    let source = active_source(host, dir, connector)?;
    let wrapped = format!("{}{GATEWAY_TOOLS}", connector_prelude(&source));
    Vm::load(host, &format!("{connector}.{EXTENSION}"), &wrapped).map_err(|e| e.to_string())
}

fn has(vm: &Vm, tool: &str) -> bool {
    vm.spec.tools.iter().any(|t| t.name == tool)
}

/// What active connector `connector`'s `gateway` table offers. An error if
/// it has none, or one that can't send.
pub async fn info(host: &Host, dir: &Path, connector: &str) -> Result<GatewayInfo, String> {
    let vm = gateway_vm(host, dir, connector)?;
    let out = parse_json(&vm.call("meta", &json!({})).await?)?;
    if out["send"] != true {
        return Err(format!(
            "connector {connector}: gateway.send must be a function"
        ));
    }
    Ok(GatewayInfo {
        kind: out["kind"]
            .as_str()
            .filter(|k| !k.is_empty())
            .unwrap_or(connector)
            .to_owned(),
        threads: out["threads"] == true,
        send: true,
        buttons: out["buttons"] == true,
        poll: out["poll"] == true,
    })
}

/// Send `text` to chat `to`. `opts` reaches the connector as its fourth
/// argument (`thread`, `kind`, `settings`, …).
pub async fn send(
    host: &Host,
    dir: &Path,
    connector: &str,
    to: &str,
    text: &str,
    opts: &Value,
) -> Result<(), String> {
    let vm = gateway_vm(host, dir, connector)?;
    vm.call("send", &json!({ "to": to, "text": text, "opts": opts }))
        .await
        .map(|_| ())
}

/// Send `text` with `buttons`. False (nothing sent) when the gateway has
/// no `buttons`: the caller falls back to text.
pub async fn buttons(
    host: &Host,
    dir: &Path,
    connector: &str,
    to: &str,
    text: &str,
    buttons: &[Button],
    opts: &Value,
) -> Result<bool, String> {
    let vm = gateway_vm(host, dir, connector)?;
    if !has(&vm, "buttons") {
        return Ok(false);
    }
    let list: Vec<Value> = buttons
        .iter()
        .map(|b| json!({ "label": b.label, "data": b.data }))
        .collect();
    vm.call(
        "buttons",
        &json!({ "to": to, "text": text, "buttons": list, "opts": opts }),
    )
    .await
    .map(|_| true)
}

/// Poll for messages after `cursor` (`null` the first time). `settings`
/// is the gateway's `[gateway.connectors.<name>] settings` table.
pub async fn poll(
    host: &Host,
    dir: &Path,
    connector: &str,
    cursor: &Value,
    settings: &Value,
) -> Result<Poll, String> {
    let vm = gateway_vm(host, dir, connector)?;
    if !has(&vm, "poll") {
        return Err(format!("connector {connector}'s gateway has no `poll`"));
    }
    let out = parse_json(
        &vm.call("poll", &json!({ "cursor": cursor, "settings": settings }))
            .await?,
    )?;
    let list = match &out["messages"] {
        Value::Array(a) => a.clone(),
        // An empty Lua table comes back as an object.
        Value::Object(o) if o.is_empty() => Vec::new(),
        _ => return Err("`messages` must be a list".into()),
    };
    if list.len() > MAX_MESSAGES {
        return Err(format!(
            "{} messages in one poll; return at most {MAX_MESSAGES} and keep the rest for the next poll with the cursor",
            list.len()
        ));
    }
    let messages = list.iter().map(inbound).collect::<Result<Vec<_>, _>>()?;
    Ok(Poll {
        messages,
        cursor: out["cursor"].clone(),
    })
}

/// A string or number field as text.
fn field(v: &Value, key: &str) -> Option<String> {
    match &v[key] {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn cut(mut s: String) -> String {
    if s.len() > MAX_TEXT {
        let mut at = MAX_TEXT;
        while !s.is_char_boundary(at) {
            at -= 1;
        }
        s.truncate(at);
        s.push_str("…[truncated]");
    }
    s
}

fn inbound(v: &Value) -> Result<Inbound, String> {
    let chat = field(v, "chat").ok_or("each message needs a `chat`")?;
    Ok(Inbound {
        id: field(v, "id"),
        chat,
        thread: field(v, "thread"),
        from: field(v, "from"),
        from_name: field(v, "from_name"),
        text: cut(field(v, "text").unwrap_or_default()),
        action: field(v, "action"),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tern_core::Secrets;
    use tern_store::Store;
    use wiremock::matchers::{body_string, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn host() -> (Host, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let secrets = tmp.path().join("secrets.toml");
        let mut host = Host::new(
            Store::open_in_memory().unwrap(),
            Secrets::load(&secrets).unwrap(),
        )
        .unwrap();
        host.timeout = Duration::from_millis(1000);
        std::fs::create_dir_all(tmp.path().join("connectors")).unwrap();
        (host, tmp)
    }

    fn install(tmp: &tempfile::TempDir, name: &str, src: &str) -> std::path::PathBuf {
        let dir = tmp.path().join("connectors");
        std::fs::write(dir.join(format!("{name}.luau")), src).unwrap();
        dir
    }

    const CHAT: &str = r#"
return {
  name = "chat",
  description = "a chat",
  capabilities = { http = { "127.0.0.1" } },
  tools = { ping = { description = "p", effect = "read", run = function() return "pong" end } },
  gateway = {
    kind = "mockchat",
    threads = true,
    send = function(ctx, to, text, opts)
      ctx.http.post(BASE .. "/send/" .. to, { body = (opts.thread or "-") .. "|" .. text })
    end,
    poll = function(ctx, cursor, settings)
      local r = ctx.http.get(BASE .. "/inbox?after=" .. tostring(cursor or 0) .. "&room=" .. settings.room).json
      return { messages = r.messages, cursor = r.cursor }
    end,
  },
}
"#;

    async fn chat() -> (Host, tempfile::TempDir, std::path::PathBuf, MockServer) {
        let server = MockServer::start().await;
        let (host, tmp) = host();
        let src = format!("local BASE = \"{}\"\n{CHAT}", server.uri());
        let dir = install(&tmp, "chat", &src);
        (host, tmp, dir, server)
    }

    #[tokio::test]
    async fn a_gateway_declares_what_it_offers_and_sends_through_its_hosts() {
        let (host, _tmp, dir, server) = chat().await;
        Mock::given(method("POST"))
            .and(path("/send/room1"))
            .and(body_string("7|hello"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let gi = info(&host, &dir, "chat").await.unwrap();
        assert_eq!(
            gi,
            GatewayInfo {
                kind: "mockchat".into(),
                threads: true,
                send: true,
                buttons: false,
                poll: true,
            }
        );
        send(
            &host,
            &dir,
            "chat",
            "room1",
            "hello",
            &json!({ "thread": "7" }),
        )
        .await
        .unwrap();
        // No `buttons`: nothing sent, the caller falls back to text.
        let b = [Button {
            label: "OK".into(),
            data: "x".into(),
        }];
        assert!(
            !buttons(&host, &dir, "chat", "room1", "hi", &b, &json!({}))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn a_poll_returns_messages_and_its_cursor() {
        let (host, _tmp, dir, server) = chat().await;
        Mock::given(method("GET"))
            .and(path("/inbox"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "messages": [
                    { "id": 1, "chat": "room1", "from": "U1", "from_name": "Ann", "text": "hi" },
                    { "id": 2, "chat": 42, "thread": "9", "action": "ap:x:once" }
                ],
                "cursor": 2
            })))
            .mount(&server)
            .await;
        let p = poll(&host, &dir, "chat", &Value::Null, &json!({ "room": "r" }))
            .await
            .unwrap();
        assert_eq!(p.cursor, json!(2));
        assert_eq!(p.messages.len(), 2);
        assert_eq!(p.messages[0].identity(), "U1");
        assert_eq!(p.messages[0].from_name.as_deref(), Some("Ann"));
        let tap = &p.messages[1];
        assert_eq!(
            (
                tap.identity(),
                tap.chat_ref().as_str(),
                tap.action.as_deref()
            ),
            ("42", "42/9", Some("ap:x:once"))
        );
    }

    #[tokio::test]
    async fn a_connector_without_a_gateway_or_send_is_refused() {
        let (host, tmp) = host();
        let dir = install(
            &tmp,
            "plain",
            r#"return { name = "plain", description = "p", tools = { x = { description = "x", effect = "read", run = function() end } } }"#,
        );
        let err = info(&host, &dir, "plain").await.unwrap_err();
        assert!(err.contains("no `gateway`"), "{err}");
        install(
            &tmp,
            "mute",
            r#"return { name = "mute", description = "m", tools = { x = { description = "x", effect = "read", run = function() end } },
              gateway = { kind = "m" } }"#,
        );
        let err = info(&host, &dir, "mute").await.unwrap_err();
        assert!(err.contains("gateway.send"), "{err}");
        let err = poll(&host, &dir, "mute", &Value::Null, &json!({}))
            .await
            .unwrap_err();
        assert!(err.contains("no `poll`"), "{err}");
    }

    #[tokio::test]
    async fn a_poll_is_read_only_and_keeps_to_its_hosts() {
        let (host, tmp) = host();
        let dir = install(
            &tmp,
            "leaky",
            r#"return { name = "leaky", description = "l", capabilities = { http = { "127.0.0.1" } },
              tools = { x = { description = "x", effect = "read", run = function() end } },
              gateway = { send = function() end,
                poll = function(ctx) ctx.http.post("http://127.0.0.1:9/x", { body = "hi" }) return { messages = {} } end } }"#,
        );
        let err = poll(&host, &dir, "leaky", &Value::Null, &json!({}))
            .await
            .unwrap_err();
        assert!(!err.is_empty());
    }
}
