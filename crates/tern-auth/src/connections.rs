//! Connections (tooling spec §6.8): several accounts per connector or MCP
//! server, each stored and authorised separately, and how a tool call is
//! routed to one. Tools aren't duplicated per account: with two or more,
//! the host adds a `connection` parameter to every tool of that target.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tern_core::tool::Effect;
use tern_store::{ConnectionRow, NewConnection, Store};

use crate::DEFAULT_SLUG;
use crate::error::AuthError;
use crate::flow::OAuthConfig;
use crate::tokens::{ConnKey, Tokens};

/// `connector` or `mcp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Connector,
    Mcp,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Connector => "connector",
            Kind::Mcp => "mcp",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "connector" => Some(Kind::Connector),
            "mcp" => Some(Kind::Mcp),
            _ => None,
        }
    }

    /// The kv namespace prefix and tool prefix used for this kind.
    fn tool_prefix(self, target: &str) -> String {
        match self {
            Kind::Connector => format!("lua__{target}__"),
            Kind::Mcp => format!("mcp__{target}__"),
        }
    }
}

/// How a connection signs in; stored as JSON on the row. Never a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum ConnAuth {
    #[serde(rename = "oauth")]
    OAuth(Box<OAuthConfig>),
    /// Keys under the connection's names (`conn/<target>/<slug>/<NAME>`,
    /// plain names for `default`).
    Key {
        #[serde(default)]
        names: Vec<String>,
    },
    None,
}

impl ConnAuth {
    pub fn method(&self) -> &'static str {
        match self {
            ConnAuth::OAuth(_) => "oauth",
            ConnAuth::Key { .. } => "key",
            ConnAuth::None => "none",
        }
    }

    pub fn oauth(&self) -> Option<&OAuthConfig> {
        match self {
            ConnAuth::OAuth(c) => Some(c.as_ref()),
            _ => None,
        }
    }
}

/// One connection, as tools and front ends see it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Conn {
    pub id: String,
    pub kind: Kind,
    pub target: String,
    pub slug: String,
    pub label: Option<String>,
    pub account: Option<String>,
    pub auth: ConnAuth,
    pub is_default: bool,
    pub status: String,
    pub last_error: Option<String>,
    pub owner_member: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

impl Conn {
    fn from_row(r: ConnectionRow) -> Option<Self> {
        let auth = r
            .auth
            .as_deref()
            .and_then(|a| serde_json::from_str(a).ok())
            .unwrap_or(match r.auth_method.as_str() {
                "key" => ConnAuth::Key { names: Vec::new() },
                _ => ConnAuth::None,
            });
        Some(Self {
            kind: Kind::parse(&r.kind)?,
            id: r.id,
            target: r.target,
            slug: r.slug,
            label: r.label,
            account: r.account,
            auth,
            is_default: r.is_default,
            status: r.status,
            last_error: r.last_error,
            owner_member: r.owner_member,
            created_at: r.created_at,
            last_used_at: r.last_used_at,
        })
    }

    pub fn key(&self) -> ConnKey {
        ConnKey::new(&self.target, &self.slug)
    }

    /// `work (mitch@inngest.com)`.
    pub fn describe(&self) -> String {
        let name = self.label.as_deref().unwrap_or(&self.slug);
        let mut out = if name == self.slug {
            self.slug.clone()
        } else {
            format!("{} \"{name}\"", self.slug)
        };
        let mut extra: Vec<String> = Vec::new();
        if let Some(a) = &self.account {
            extra.push(a.clone());
        }
        if self.is_default {
            extra.push("default".into());
        }
        if self.status != "ok" {
            extra.push(self.status.replace('_', " "));
        }
        if !extra.is_empty() {
            out.push_str(&format!(" ({})", extra.join(", ")));
        }
        out
    }

    /// For approval cards and the ledger: `gmail · personal
    /// (mitch@gmail.com)`.
    pub fn title(&self) -> String {
        match &self.account {
            Some(a) => format!("{} · {} ({a})", self.target, self.slug),
            None => format!("{} · {}", self.target, self.slug),
        }
    }
}

/// The account a call runs under. `None` from [`Connections::resolve`]
/// means the target has no connections yet: the implicit `default`, with
/// plain secret names, as before connections existed.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub conn: Conn,
}

impl Resolved {
    pub fn slug(&self) -> &str {
        &self.conn.slug
    }
}

/// The name a target's tools give the account parameter: `connection`, or
/// `tern_connection` when a tool already has its own `connection`.
pub fn param_name(schema: &Value) -> &'static str {
    if schema
        .get("properties")
        .and_then(|p| p.get("connection"))
        .is_some()
    {
        "tern_connection"
    } else {
        "connection"
    }
}

/// `schema` with the account parameter added (§6.8.2): an enum of slugs,
/// each described with its account.
pub fn with_param(schema: &Value, conns: &[Conn]) -> Value {
    let name = param_name(schema);
    let mut s = schema.clone();
    let described: Vec<String> = conns.iter().map(Conn::describe).collect();
    let prop = json!({
        "type": "string",
        "enum": conns.iter().map(|c| c.slug.clone()).collect::<Vec<_>>(),
        "description": format!(
            "Which account to use: {}. Reads without it use the default; writes must name one.",
            described.join("; ")
        ),
    });
    if let Some(obj) = s.as_object_mut() {
        let props = obj
            .entry("properties")
            .or_insert_with(|| Value::Object(Default::default()));
        if let Some(p) = props.as_object_mut() {
            p.insert(name.to_owned(), prop);
        }
    }
    s
}

/// Take the account parameter out of `args` (the tool itself never sees
/// it). Returns the slug asked for.
pub fn take_param(schema: &Value, args: &mut Value) -> Option<String> {
    let name = param_name(schema);
    let v = args.as_object_mut()?.remove(name)?;
    v.as_str().map(str::to_owned).filter(|s| !s.is_empty())
}

/// What front ends and tools work with.
pub struct Connections {
    store: Store,
    pub tokens: Arc<Tokens>,
    /// The latest rows by (kind, target), for building tool specs without
    /// a query per tool.
    cache: RwLock<BTreeMap<(Kind, String), Vec<Conn>>>,
    pub max_per_target: usize,
    /// Told after every change this process makes.
    on_change: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
}

impl Connections {
    pub fn new(store: Store, tokens: Arc<Tokens>, max_per_target: usize) -> Self {
        let c = Self {
            store,
            tokens,
            cache: RwLock::default(),
            max_per_target: max_per_target.max(1),
            on_change: std::sync::OnceLock::new(),
        };
        c.reload();
        c
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Call `f` after every change (for the API's events).
    pub fn on_change(&self, f: impl Fn() + Send + Sync + 'static) {
        let _ = self.on_change.set(Box::new(f));
    }

    fn changed(&self) {
        self.reload();
        if let Some(f) = self.on_change.get() {
            f();
        }
    }

    /// Re-read every connection (after a change from anywhere).
    pub fn reload(&self) {
        let rows = self.store.all_connections().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "reading connections failed");
            Vec::new()
        });
        let mut map: BTreeMap<(Kind, String), Vec<Conn>> = BTreeMap::new();
        for c in rows.into_iter().filter_map(Conn::from_row) {
            map.entry((c.kind, c.target.clone())).or_default().push(c);
        }
        if let Ok(mut g) = self.cache.write() {
            *g = map;
        }
    }

    /// A target's connections, default first.
    pub fn list(&self, kind: Kind, target: &str) -> Vec<Conn> {
        self.cache
            .read()
            .ok()
            .and_then(|m| m.get(&(kind, target.to_owned())).cloned())
            .unwrap_or_default()
    }

    pub fn all(&self) -> Vec<Conn> {
        self.cache
            .read()
            .map(|m| m.values().flatten().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get(&self, kind: Kind, target: &str, slug: &str) -> Option<Conn> {
        self.list(kind, target).into_iter().find(|c| c.slug == slug)
    }

    /// The prompt's Integrations line, when a target has connections:
    /// `work (mitch@inngest.com, default), personal (mitch@gmail.com)`.
    pub fn describe(&self, kind: Kind, target: &str) -> Option<String> {
        let conns = self.list(kind, target);
        (!conns.is_empty()).then(|| {
            conns
                .iter()
                .map(Conn::describe)
                .collect::<Vec<_>>()
                .join(", ")
        })
    }

    /// Whether the target's tools get an account parameter.
    pub fn routed(&self, kind: Kind, target: &str) -> Option<Vec<Conn>> {
        let conns = self.list(kind, target);
        (conns.len() >= 2).then_some(conns)
    }

    /// The account for a call (§6.8.2): the one asked for; else the only
    /// one; else the default for a read. A write or destructive call with
    /// more than one account and none named is refused with the options,
    /// so the model has to choose. `Ok(None)`: no connections yet.
    pub fn resolve(
        &self,
        kind: Kind,
        target: &str,
        asked: Option<&str>,
        effect: Effect,
    ) -> Result<Option<Resolved>, String> {
        let conns = self.list(kind, target);
        if conns.is_empty() {
            return match asked {
                None | Some(DEFAULT_SLUG) => Ok(None),
                Some(s) => Err(format!(
                    "{target} has no connection {s:?}; it has only its default account"
                )),
            };
        }
        let options = || {
            conns
                .iter()
                .map(Conn::describe)
                .collect::<Vec<_>>()
                .join("; ")
        };
        let conn = match asked {
            Some(s) => conns.iter().find(|c| c.slug == s).ok_or_else(|| {
                format!("{target} has no connection {s:?}. Options: {}", options())
            })?,
            None if conns.len() == 1 => &conns[0],
            None if effect == Effect::Read => {
                conns.iter().find(|c| c.is_default).unwrap_or(&conns[0])
            }
            None => {
                return Err(format!(
                    "{target} has more than one account, so this {effect} call must say which: pass `connection` as one of: {}. Nothing was sent.",
                    options()
                ));
            }
        };
        if conn.status == "needs_reauth" {
            return Err(format!(
                "{} needs signing in again ({}): ask the user to reconnect it (Connections in the app, /connections in the TUI, or `tern connections reconnect {} {}`)",
                conn.title(),
                conn.last_error.as_deref().unwrap_or("its sign-in expired"),
                target,
                conn.slug
            ));
        }
        let _ = self.store.touch_connection(&conn.id);
        Ok(Some(Resolved { conn: conn.clone() }))
    }

    /// An access token for an OAuth connection; a dead grant marks it
    /// `needs_reauth`. `rejected` is a token the service just refused.
    pub async fn access_token(
        &self,
        r: &Resolved,
        rejected: Option<&str>,
    ) -> Result<String, String> {
        let Some(cfg) = r.conn.auth.oauth() else {
            return Err(format!("{} doesn't sign in with OAuth", r.conn.title()));
        };
        match self.tokens.access_token(&r.conn.key(), cfg, rejected).await {
            Ok(t) => Ok(t),
            Err(AuthError::NeedsReauth(why)) => {
                let why = crate::error::short(&why, 200);
                if let Err(e) =
                    self.store
                        .set_connection_status(&r.conn.id, "needs_reauth", Some(&why))
                {
                    tracing::warn!(error = %e, "marking a connection for sign-in failed");
                }
                self.changed();
                tracing::warn!(target = %r.conn.target, connection = %r.conn.slug, "connection needs sign-in again");
                Err(format!(
                    "{} needs signing in again ({why}): ask the user to reconnect it",
                    r.conn.title()
                ))
            }
            Err(e) => Err(format!("{}: {e}", r.conn.title())),
        }
    }

    /// Where a declared secret lives for a call's account.
    pub fn secret_name(r: Option<&Resolved>, name: &str) -> String {
        match r {
            Some(r) => r.conn.key().secret_name(name),
            None => name.to_owned(),
        }
    }

    /// A connector's kv namespace for a call's account: `lua:<name>` for
    /// the default (so existing caches and cursors carry over), else
    /// `lua:<name>:<slug>`.
    pub fn kv_namespace(connector: &str, r: Option<&Resolved>) -> String {
        match r {
            Some(r) if r.conn.slug != DEFAULT_SLUG => format!("lua:{connector}:{}", r.conn.slug),
            _ => format!("lua:{connector}"),
        }
    }

    /// The suffix a standing grant gets when it is for one account, so a
    /// grant for *work* doesn't cover *personal* (§6.8.1).
    pub fn grant_suffix(r: Option<&Resolved>) -> String {
        match r {
            Some(r) => format!("@{}", r.conn.slug),
            None => String::new(),
        }
    }

    /// Check a new slug for `target`.
    pub fn check_new(&self, kind: Kind, target: &str, slug: &str) -> Result<(), String> {
        if !tern_core::secrets::valid_slug(slug) {
            return Err(format!(
                "connection names are [a-z0-9_-], 1 to 16 characters, not {slug:?}"
            ));
        }
        let conns = self.list(kind, target);
        if conns.iter().any(|c| c.slug == slug) {
            return Err(format!("{target} already has a connection {slug:?}"));
        }
        if conns.len() >= self.max_per_target {
            return Err(format!(
                "{target} already has {} connections, the limit ([auth] max_connections)",
                conns.len()
            ));
        }
        Ok(())
    }

    /// Record a connection.
    pub fn add(
        &self,
        kind: Kind,
        target: &str,
        slug: &str,
        label: Option<&str>,
        account: Option<&str>,
        auth: &ConnAuth,
    ) -> Result<Conn, String> {
        self.check_new(kind, target, slug)?;
        let row = self
            .store
            .add_connection(&NewConnection {
                kind: kind.as_str().into(),
                target: target.into(),
                slug: slug.into(),
                label: label.map(Into::into),
                account: account.map(Into::into),
                auth_method: auth.method().into(),
                auth: serde_json::to_string(auth).ok(),
            })
            .map_err(|e| e.to_string())?;
        self.changed();
        Conn::from_row(row).ok_or_else(|| "bad connection row".into())
    }

    /// A sign-in finished for an existing connection (Reconnect).
    pub fn signed_in(
        &self,
        c: &Conn,
        account: Option<&str>,
        auth: Option<&ConnAuth>,
    ) -> Result<(), String> {
        let auth = auth.and_then(|a| serde_json::to_string(a).ok());
        self.store
            .update_connection_auth(&c.id, account, auth.as_deref())
            .map_err(|e| e.to_string())?;
        self.changed();
        Ok(())
    }

    /// The connection already signed in as `account`, if any (same account
    /// twice offers Reconnect instead of a duplicate).
    pub fn with_account(&self, kind: Kind, target: &str, account: &str) -> Option<Conn> {
        self.list(kind, target).into_iter().find(|c| {
            c.account
                .as_deref()
                .is_some_and(|a| a.eq_ignore_ascii_case(account))
        })
    }

    pub fn set_default(&self, c: &Conn) -> Result<(), String> {
        self.store
            .set_default_connection(&c.id)
            .map_err(|e| e.to_string())?;
        self.changed();
        Ok(())
    }

    pub fn rename(&self, c: &Conn, label: &str) -> Result<(), String> {
        let label = crate::error::short(label, 60);
        self.store
            .rename_connection(&c.id, &label)
            .map_err(|e| e.to_string())?;
        self.changed();
        Ok(())
    }

    /// Remove (§6.8.3): revoke at the provider when it can, delete the
    /// connection's secrets, kv and grants. Plain-named secrets of a
    /// `default` connection stay (other things may use them).
    pub async fn remove(&self, c: &Conn) -> Result<(), String> {
        self.tokens
            .forget(&c.key(), c.auth.oauth())
            .await
            .map_err(|e| e.to_string())?;
        if c.kind == Kind::Connector && c.slug != DEFAULT_SLUG {
            let r = Resolved { conn: c.clone() };
            let _ = self
                .store
                .kv_clear(&Self::kv_namespace(&c.target, Some(&r)));
        }
        let _ = self
            .store
            .remove_allowlist_matching(&c.kind.tool_prefix(&c.target), &format!("@{}", c.slug));
        self.store
            .remove_connection(&c.id)
            .map_err(|e| e.to_string())?;
        self.changed();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Connections) {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let secrets =
            tern_core::Secrets::load(&dir.path().join("secrets.toml")).unwrap_or_default();
        let store = Store::open_in_memory().unwrap_or_else(|_| unreachable!());
        let tokens = Arc::new(Tokens::new(secrets, crate::Guard::offline()));
        (dir, Connections::new(store, tokens, 3))
    }

    #[test]
    fn auth_is_stored_as_tagged_json() {
        let a = ConnAuth::OAuth(Box::default());
        let j = serde_json::to_value(&a).unwrap_or_default();
        assert_eq!(j["method"], "oauth");
        assert_eq!(serde_json::from_value::<ConnAuth>(j).ok(), Some(a));
        let k = serde_json::to_value(ConnAuth::Key { names: vec!["K".into()] }).unwrap_or_default();
        assert_eq!(k, json!({"method": "key", "names": ["K"]}));
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn routing_reads_default_and_writes_must_choose() {
        let (_d, c) = setup();
        let k = Kind::Connector;
        assert_eq!(
            c.resolve(k, "gmail", None, Effect::Write),
            Ok(None),
            "no connections: legacy"
        );
        let key = ConnAuth::Key { names: vec![] };
        c.add(k, "gmail", "work", None, Some("me@work.com"), &key)
            .unwrap();
        let only = c.resolve(k, "gmail", None, Effect::Write).unwrap().unwrap();
        assert_eq!(only.slug(), "work", "one account: no choice needed");
        c.add(
            k,
            "gmail",
            "personal",
            Some("Personal"),
            Some("me@home.com"),
            &key,
        )
        .unwrap();
        assert_eq!(
            c.resolve(k, "gmail", None, Effect::Read)
                .unwrap()
                .unwrap()
                .slug(),
            "work"
        );
        let err = c.resolve(k, "gmail", None, Effect::Write).unwrap_err();
        assert!(
            err.contains("must say which")
                && err.contains("personal")
                && err.contains("me@home.com"),
            "{err}"
        );
        assert_eq!(
            c.resolve(k, "gmail", Some("personal"), Effect::Destructive)
                .unwrap()
                .unwrap()
                .slug(),
            "personal"
        );
        assert!(c.resolve(k, "gmail", Some("nope"), Effect::Read).is_err());
        assert!(c.check_new(k, "gmail", "Bad Name").is_err());
        assert!(c.check_new(k, "gmail", "work").is_err());
        c.add(k, "gmail", "third", None, None, &key).unwrap();
        assert!(
            c.check_new(k, "gmail", "fourth")
                .unwrap_err()
                .contains("limit")
        );
        let schema = json!({"type": "object", "properties": {"to": {"type": "string"}}});
        let conns = c.routed(k, "gmail").unwrap();
        let s = with_param(&schema, &conns);
        assert_eq!(
            s["properties"]["connection"]["enum"],
            json!(["work", "personal", "third"])
        );
        let own = json!({"type": "object", "properties": {"connection": {"type": "string"}}});
        assert!(with_param(&own, &conns)["properties"]["tern_connection"].is_object());
        let mut args = json!({"to": "a", "connection": "personal"});
        assert_eq!(take_param(&schema, &mut args).as_deref(), Some("personal"));
        assert_eq!(args, json!({"to": "a"}));
        assert_eq!(
            c.describe(k, "gmail").unwrap(),
            "work (me@work.com, default), personal \"Personal\" (me@home.com), third"
        );
        let p = c.get(k, "gmail", "personal").unwrap();
        assert_eq!(
            Connections::kv_namespace("gmail", Some(&Resolved { conn: p.clone() })),
            "lua:gmail:personal"
        );
        assert_eq!(Connections::kv_namespace("gmail", None), "lua:gmail");
        assert_eq!(
            c.with_account(k, "gmail", "ME@home.com").map(|x| x.slug),
            Some("personal".into())
        );
    }
}
