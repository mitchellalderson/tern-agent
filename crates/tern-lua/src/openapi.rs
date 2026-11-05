//! A Luau connector from an OpenAPI 3.x or Swagger 2.0 document (tooling
//! spec §6.6, T4c): one tool per chosen operation, with typed params, an
//! effect from the method, exact host and path allowlists from the servers
//! and paths, `auth` from the security schemes, and a rate when the document
//! states one. The result is an ordinary draft: it is tested and activated
//! like a hand-written one.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use serde_json::{Map, Value, json};
use tern_core::config::Rate;

use crate::spec::valid_ident;

/// Largest document read, JSON or YAML.
pub const MAX_DOCUMENT: usize = 25 * 1024 * 1024;
/// Most JSON nodes a document may expand to (YAML aliases included).
const MAX_NODES: usize = 4_000_000;
/// How deep `$ref`s and nested schemas are followed into a tool's params.
const SCHEMA_DEPTH: usize = 4;
/// Most `capabilities.http` entries before they collapse to the base path.
const MAX_ALLOW_ENTRIES: usize = 40;
const METHODS: [&str; 6] = ["get", "head", "post", "put", "patch", "delete"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loc {
    Path,
    Query,
    Header,
}

impl Loc {
    fn as_str(self) -> &'static str {
        match self {
            Loc::Path => "path",
            Loc::Query => "query",
            Loc::Header => "header",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Param {
    /// The name the API knows it by.
    pub name: String,
    /// The tool argument carrying it (the name, unless two collide).
    pub arg: String,
    pub loc: Loc,
    pub required: bool,
    /// Arrays in the query repeat the key (`a=1&a=2`) rather than join
    /// with commas.
    pub explode: bool,
    pub schema: Value,
}

#[derive(Debug, Clone)]
pub struct Body {
    pub arg: String,
    /// `application/x-www-form-urlencoded` rather than JSON.
    pub form: bool,
    pub required: bool,
    pub schema: Value,
}

#[derive(Debug, Clone)]
pub struct Operation {
    /// The tool's name: unique, `[a-z][a-z0-9_]*`, at most 32 characters.
    pub key: String,
    pub operation_id: Option<String>,
    /// Uppercase.
    pub method: String,
    /// Relative to the base URL, as written in the document.
    pub path: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub deprecated: bool,
    pub params: Vec<Param>,
    pub body: Option<Body>,
    /// The operation's own `security`, else the document's.
    pub security: Vec<Requirement>,
    /// Why it can't be generated, if it can't.
    pub unsupported: Option<String>,
}

impl Operation {
    pub fn is_read(&self) -> bool {
        matches!(self.method.as_str(), "GET" | "HEAD")
    }

    pub fn effect(&self) -> &'static str {
        match self.method.as_str() {
            "GET" | "HEAD" => "read",
            "DELETE" => "destructive",
            _ => "write",
        }
    }

    fn line(&self) -> String {
        let mut out = format!(
            "{} [{}] {} {}",
            self.key,
            self.effect(),
            self.method,
            self.path
        );
        if !self.summary.is_empty() {
            out.push_str(&format!(" — {}", short(&self.summary, 100)));
        }
        if self.deprecated {
            out.push_str(" (deprecated)");
        }
        if let Some(why) = &self.unsupported {
            out.push_str(&format!(" (can't generate: {why})"));
        }
        out
    }
}

/// One way to satisfy an operation's security: every scheme named, each
/// with the scopes it needs.
pub type Requirement = Vec<(String, Vec<String>)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scheme {
    Bearer,
    Basic,
    ApiKeyHeader(String),
    ApiKeyQuery(String),
    OAuth2 {
        authorize_url: String,
        token_url: String,
        scopes: Vec<String>,
    },
    /// OAuth2 with only flows a user can't sign in with (client
    /// credentials, password): a token the user supplies.
    OAuth2Token,
    OpenIdConnect(String),
    Unsupported(String),
}

#[derive(Debug, Clone)]
pub struct Document {
    pub title: String,
    pub description: String,
    /// `3.0.3`, `3.1.0`, `2.0`.
    pub version: String,
    /// Where requests go: scheme, host, port and base path, no trailing `/`.
    pub base_url: String,
    pub operations: Vec<Operation>,
    pub schemes: BTreeMap<String, Scheme>,
    pub rate: Option<Rate>,
}

impl Document {
    pub fn host(&self) -> String {
        reqwest::Url::parse(&self.base_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default()
    }

    fn base_path(&self) -> String {
        reqwest::Url::parse(&self.base_url)
            .map(|u| u.path().trim_end_matches('/').to_owned())
            .unwrap_or_default()
    }

    /// Operation counts by tag, most first.
    pub fn tags(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for op in &self.operations {
            if op.tags.is_empty() {
                *counts.entry("(untagged)").or_default() += 1;
            }
            for t in &op.tags {
                *counts.entry(t.as_str()).or_default() += 1;
            }
        }
        let mut v: Vec<(String, usize)> =
            counts.into_iter().map(|(k, n)| (k.to_owned(), n)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    /// A connector name from the title (`Swagger Petstore` → `swagger_petstore`).
    pub fn suggested_name(&self) -> String {
        let mut name = snake(&self.title);
        if name.is_empty() {
            let host = self.host();
            name = snake(
                host.split('.')
                    .find(|l| *l != "api" && *l != "www")
                    .unwrap_or("api"),
            );
        }
        fit_ident(&name, "api")
    }
}

// ---- parsing ----

/// Parse a document (JSON or YAML). `source` is where it came from, to
/// resolve a relative server URL; `base_url` overrides the servers.
pub fn parse(text: &str, source: Option<&str>, base_url: Option<&str>) -> Result<Document, String> {
    if text.len() > MAX_DOCUMENT {
        return Err(format!("the document is larger than {MAX_DOCUMENT} bytes"));
    }
    let root = if text.trim_start().starts_with('{') {
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?
    } else {
        from_yaml(text)?
    };
    let Some(obj) = root.as_object() else {
        return Err("the document isn't an object".into());
    };
    let version = if let Some(v) = obj.get("openapi").and_then(scalar_str) {
        if !v.starts_with('3') {
            return Err(format!(
                "OpenAPI {v} isn't supported (3.x and Swagger 2.0 are)"
            ));
        }
        v
    } else if let Some(v) = obj.get("swagger").and_then(scalar_str) {
        if v != "2.0" {
            return Err(format!("Swagger {v} isn't supported (2.0 is)"));
        }
        v
    } else {
        return Err(
            "this isn't an OpenAPI or Swagger document (no `openapi` or `swagger` field)".into(),
        );
    };
    let swagger = version == "2.0";
    let info = obj.get("info").cloned().unwrap_or(Value::Null);
    let title = info["title"].as_str().unwrap_or_default().trim().to_owned();
    let description = info["description"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_owned();
    let base_url = match base_url {
        Some(b) => absolute(b, source)?,
        None if swagger => swagger_base(&root, source)?,
        None => openapi_base(&root, source)?,
    };
    let schemes = security_schemes(&root, swagger);
    let global_security = requirements(root.get("security"));
    let rate = stated_rate(&root);

    let mut operations = Vec::new();
    let mut taken = BTreeSet::new();
    if let Some(paths) = obj.get("paths").and_then(Value::as_object) {
        for (path, item) in paths {
            let item = deref(&root, item);
            let shared = item.get("parameters").and_then(Value::as_array);
            for method in METHODS {
                let Some(op) = item.get(method) else {
                    continue;
                };
                let op = deref(&root, op);
                operations.push(operation(
                    &root,
                    swagger,
                    method,
                    path,
                    op,
                    shared,
                    &global_security,
                    &mut taken,
                ));
            }
        }
    }
    if operations.is_empty() {
        return Err("the document has no operations under `paths`".into());
    }
    Ok(Document {
        title,
        description,
        version,
        base_url,
        operations,
        schemes,
        rate,
    })
}

/// A scalar as a string (`swagger: 2.0` unquoted in YAML is a number).
fn scalar_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn absolute(url: &str, source: Option<&str>) -> Result<String, String> {
    let parsed = match reqwest::Url::parse(url) {
        Ok(u) => u,
        Err(_) => {
            let src = source
                .and_then(|s| reqwest::Url::parse(s).ok())
                .filter(|u| matches!(u.scheme(), "http" | "https"))
                .ok_or_else(|| {
                    format!(
                        "the server URL {url:?} is relative and the document wasn't fetched from a URL; pass base_url"
                    )
                })?;
            src.join(url)
                .map_err(|e| format!("server URL {url:?}: {e}"))?
        }
    };
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(format!(
            "the server URL {url:?} isn't an http(s) URL with a host"
        ));
    }
    let mut s = format!(
        "{}://{}{}",
        parsed.scheme(),
        parsed.host_str().unwrap_or_default(),
        parsed.port().map(|p| format!(":{p}")).unwrap_or_default()
    );
    s.push_str(parsed.path().trim_end_matches('/'));
    Ok(s)
}

fn openapi_base(root: &Value, source: Option<&str>) -> Result<String, String> {
    let servers = root["servers"].as_array().cloned().unwrap_or_default();
    // Prefer an https server; otherwise the first.
    let pick = servers
        .iter()
        .find(|s| s["url"].as_str().is_some_and(|u| u.starts_with("https://")))
        .or_else(|| servers.first());
    let Some(server) = pick else {
        return absolute("/", source);
    };
    let mut url = server["url"].as_str().unwrap_or("/").to_owned();
    if let Some(vars) = server["variables"].as_object() {
        for (k, v) in vars {
            let default = v.get("default").and_then(scalar_str).unwrap_or_default();
            url = url.replace(&format!("{{{k}}}"), &default);
        }
    }
    if url.contains('{') {
        return Err(format!(
            "the server URL {url:?} has variables without defaults; pass base_url"
        ));
    }
    absolute(&url, source)
}

fn swagger_base(root: &Value, source: Option<&str>) -> Result<String, String> {
    let schemes: Vec<&str> = root["schemes"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let base_path = root["basePath"].as_str().unwrap_or("/");
    match root["host"].as_str() {
        Some(host) => {
            let scheme = if schemes.is_empty() || schemes.contains(&"https") {
                "https"
            } else {
                "http"
            };
            absolute(&format!("{scheme}://{host}{base_path}"), source)
        }
        None => absolute(base_path, source),
    }
}

/// Follow a local `$ref` (`#/components/...`, `#/definitions/...`) one
/// level; anything else is returned as is.
fn deref<'a>(root: &'a Value, v: &'a Value) -> &'a Value {
    let mut cur = v;
    for _ in 0..8 {
        match cur.get("$ref").and_then(Value::as_str) {
            Some(r) if r.starts_with("#/") => match root.pointer(&r[1..]) {
                Some(t) => cur = t,
                None => return &Value::Null,
            },
            _ => return cur,
        }
    }
    cur
}

#[allow(clippy::too_many_arguments)]
fn operation(
    root: &Value,
    swagger: bool,
    method: &str,
    path: &str,
    op: &Value,
    shared: Option<&Vec<Value>>,
    global_security: &[Requirement],
    taken: &mut BTreeSet<String>,
) -> Operation {
    let operation_id = op["operationId"].as_str().map(str::to_owned);
    let key = unique_key(
        &operation_id
            .as_deref()
            .map(snake)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| key_from_path(method, path)),
        taken,
    );
    let summary = op["summary"]
        .as_str()
        .or_else(|| op["description"].as_str())
        .map(|s| s.lines().next().unwrap_or_default().trim().to_owned())
        .unwrap_or_default();
    let tags = op["tags"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let security = match op.get("security") {
        Some(s) => requirements(Some(s)),
        None => global_security.to_vec(),
    };

    // Operation parameters override the path item's by (name, in).
    let mut raw: Vec<&Value> = Vec::new();
    let own: Vec<&Value> = op["parameters"]
        .as_array()
        .map(|a| a.iter().map(|p| deref(root, p)).collect())
        .unwrap_or_default();
    for p in shared.into_iter().flatten().map(|p| deref(root, p)) {
        let same = |q: &&Value| q["name"] == p["name"] && q["in"] == p["in"];
        if !own.iter().any(same) {
            raw.push(p);
        }
    }
    raw.extend(own);

    let mut params = Vec::new();
    let mut body = None;
    let mut unsupported = None;
    let mut args = BTreeSet::new();
    let mut form_fields: Vec<(String, Value, bool)> = Vec::new();
    for p in raw {
        let name = p["name"].as_str().unwrap_or_default().to_owned();
        let loc = match p["in"].as_str() {
            Some("path") => Loc::Path,
            Some("query") => Loc::Query,
            Some("header") => Loc::Header,
            Some("body") if swagger => {
                body = Some(Body {
                    arg: String::new(),
                    form: false,
                    required: p["required"].as_bool().unwrap_or(false),
                    schema: slim(root, &p["schema"], 0),
                });
                continue;
            }
            Some("formData") if swagger => {
                if p["type"] == "file" {
                    unsupported = Some("file uploads".to_owned());
                }
                form_fields.push((
                    name,
                    slim(root, p, 0),
                    p["required"].as_bool().unwrap_or(false),
                ));
                continue;
            }
            // Cookies can't be set by a connector.
            Some("cookie") => {
                if p["required"].as_bool().unwrap_or(false) {
                    unsupported = Some(format!("a required cookie ({name})"));
                }
                continue;
            }
            _ => continue,
        };
        if name.is_empty() {
            continue;
        }
        let schema = if swagger {
            slim(root, p, 0)
        } else {
            slim(root, &p["schema"], 0)
        };
        let mut schema = schema;
        if let Some(d) = p["description"].as_str() {
            schema["description"] = short(d.trim(), 200).into();
        }
        let explode = if swagger {
            p["collectionFormat"].as_str() == Some("multi")
        } else {
            p["explode"]
                .as_bool()
                .unwrap_or(p["style"].as_str().is_none_or(|s| s == "form"))
        };
        let arg = if args.insert(name.clone()) {
            name.clone()
        } else {
            let alt = format!("{}_{name}", loc.as_str());
            args.insert(alt.clone());
            alt
        };
        params.push(Param {
            required: loc == Loc::Path || p["required"].as_bool().unwrap_or(false),
            name,
            arg,
            loc,
            explode,
            schema,
        });
    }
    if !form_fields.is_empty() {
        let mut props = Map::new();
        let mut req = Vec::new();
        for (n, s, r) in form_fields {
            if r {
                req.push(Value::String(n.clone()));
            }
            props.insert(n, s);
        }
        let mut schema = json!({ "type": "object", "properties": props });
        let required = !req.is_empty();
        if required {
            schema["required"] = Value::Array(req);
        }
        body = Some(Body {
            arg: String::new(),
            form: true,
            required,
            schema,
        });
    }
    if !swagger && let Some(rb) = op.get("requestBody") {
        let rb = deref(root, rb);
        let content = rb["content"].as_object();
        let json_type = content.and_then(|c| {
            c.iter()
                .find(|(k, _)| k.starts_with("application/json") || k.ends_with("+json"))
        });
        let form_type = content.and_then(|c| c.get("application/x-www-form-urlencoded"));
        let required = rb["required"].as_bool().unwrap_or(false);
        if let Some((_, media)) = json_type {
            body = Some(Body {
                arg: String::new(),
                form: false,
                required,
                schema: slim(root, &media["schema"], 0),
            });
        } else if let Some(media) = form_type {
            body = Some(Body {
                arg: String::new(),
                form: true,
                required,
                schema: slim(root, &media["schema"], 0),
            });
        } else if let Some(kind) = content.and_then(|c| c.keys().next())
            && required
        {
            unsupported = Some(format!("a {kind} request body"));
        }
    }
    if let Some(b) = &mut body {
        b.arg = ["body", "request_body", "payload"]
            .into_iter()
            .find(|n| !args.contains(*n))
            .unwrap_or("request_body_")
            .to_owned();
    }
    Operation {
        key,
        operation_id,
        method: method.to_ascii_uppercase(),
        path: path.to_owned(),
        summary,
        tags,
        deprecated: op["deprecated"].as_bool().unwrap_or(false),
        params,
        body,
        security,
        unsupported,
    }
}

fn requirements(v: Option<&Value>) -> Vec<Requirement> {
    v.and_then(Value::as_array)
        .map(|alts| {
            alts.iter()
                .filter_map(Value::as_object)
                .map(|req| {
                    req.iter()
                        .map(|(name, scopes)| {
                            let scopes = scopes
                                .as_array()
                                .map(|a| {
                                    a.iter()
                                        .filter_map(Value::as_str)
                                        .map(str::to_owned)
                                        .collect()
                                })
                                .unwrap_or_default();
                            (name.clone(), scopes)
                        })
                        .collect()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn security_schemes(root: &Value, swagger: bool) -> BTreeMap<String, Scheme> {
    let defs = if swagger {
        &root["securityDefinitions"]
    } else {
        &root["components"]["securitySchemes"]
    };
    let mut out = BTreeMap::new();
    for (name, s) in defs.as_object().into_iter().flatten() {
        let s = deref(root, s);
        let in_ = s["in"].as_str().unwrap_or_default();
        let key_name = s["name"].as_str().unwrap_or_default().to_owned();
        let scheme = match s["type"].as_str().unwrap_or_default() {
            "http" => match s["scheme"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str()
            {
                "bearer" => Scheme::Bearer,
                "basic" => Scheme::Basic,
                other => Scheme::Unsupported(format!("HTTP {other} authentication")),
            },
            "basic" => Scheme::Basic,
            "apiKey" if in_ == "header" && !key_name.is_empty() => Scheme::ApiKeyHeader(key_name),
            "apiKey" if in_ == "query" && !key_name.is_empty() => Scheme::ApiKeyQuery(key_name),
            "apiKey" => Scheme::Unsupported(format!("an API key in a {in_}")),
            "oauth2" => oauth2(s, swagger),
            "openIdConnect" => Scheme::OpenIdConnect(
                s["openIdConnectUrl"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            other => Scheme::Unsupported(format!("{other:?} authentication")),
        };
        out.insert(name.clone(), scheme);
    }
    out
}

fn oauth2(s: &Value, swagger: bool) -> Scheme {
    let scopes_of = |v: &Value| -> Vec<String> {
        v.as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    };
    // A flow a person signs in with: authorization code (OAS2 "accessCode"),
    // then implicit, which tern runs as a code flow with PKCE.
    let flows: Vec<(String, String, Vec<String>)> = if swagger {
        match s["flow"].as_str() {
            Some("accessCode") | Some("implicit") => vec![(
                s["authorizationUrl"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                s["tokenUrl"].as_str().unwrap_or_default().to_owned(),
                scopes_of(&s["scopes"]),
            )],
            _ => Vec::new(),
        }
    } else {
        ["authorizationCode", "implicit"]
            .iter()
            .filter_map(|f| s["flows"].get(*f))
            .map(|f| {
                (
                    f["authorizationUrl"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    f["tokenUrl"].as_str().unwrap_or_default().to_owned(),
                    scopes_of(&f["scopes"]),
                )
            })
            .collect()
    };
    match flows.into_iter().find(|(a, _, _)| !a.is_empty()) {
        Some((authorize_url, token_url, scopes)) => Scheme::OAuth2 {
            authorize_url,
            token_url,
            scopes,
        },
        None => Scheme::OAuth2Token,
    }
}

/// A rate the document states: an `x-rate-limit`-style extension on the
/// root or `info` ("60/min", a number per minute, or `{ limit, period }`),
/// else "N requests per minute" in `info.description`.
fn stated_rate(root: &Value) -> Option<Rate> {
    const KEYS: [&str; 5] = [
        "x-rate-limit",
        "x-ratelimit",
        "x-rate-limits",
        "x-ratelimit-limit",
        "x-rateLimit",
    ];
    for holder in [root, &root["info"]] {
        for k in KEYS {
            if let Some(r) = holder.get(k).and_then(rate_value) {
                return Some(r);
            }
        }
    }
    root["info"]["description"].as_str().and_then(rate_in_text)
}

fn rate_value(v: &Value) -> Option<Rate> {
    match v {
        Value::String(s) => s.parse().ok().or_else(|| rate_in_text(s)),
        Value::Number(n) => format!("{}/min", n.as_u64()?).parse().ok(),
        Value::Object(o) => {
            let count = ["limit", "requests", "count", "rate"]
                .iter()
                .find_map(|k| o.get(*k)?.as_u64())?;
            let per = ["period", "per", "window", "interval", "unit"]
                .iter()
                .find_map(|k| o.get(*k))?;
            let unit = match per {
                Value::Number(n) => match n.as_u64()? {
                    1 => "sec",
                    60 => "min",
                    3600 => "hour",
                    _ => return None,
                },
                Value::String(s) => unit_word(s)?,
                _ => return None,
            };
            format!("{count}/{unit}").parse().ok()
        }
        Value::Array(a) => a.iter().find_map(rate_value),
        _ => None,
    }
}

fn unit_word(s: &str) -> Option<&'static str> {
    match s.trim().trim_end_matches('.').to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => Some("sec"),
        "m" | "min" | "mins" | "minute" | "minutes" => Some("min"),
        "h" | "hr" | "hour" | "hours" => Some("hour"),
        _ => None,
    }
}

/// "100 requests per minute", "60 calls/min", "10 requests a second".
fn rate_in_text(text: &str) -> Option<Rate> {
    let words: Vec<String> = text
        .split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')')
        .filter(|w| !w.is_empty())
        .flat_map(|w| {
            // "requests/minute" → "requests", "/", "minute"
            let mut parts = Vec::new();
            for (i, p) in w.split('/').enumerate() {
                if i > 0 {
                    parts.push("/".to_owned());
                }
                if !p.is_empty() {
                    parts.push(p.to_ascii_lowercase());
                }
            }
            parts
        })
        .collect();
    for (i, w) in words.iter().enumerate() {
        let Ok(count) = w.replace('_', "").parse::<u32>() else {
            continue;
        };
        let rest = &words[i + 1..];
        let noun = rest.first().map(String::as_str);
        if !matches!(
            noun,
            Some("requests" | "request" | "calls" | "call" | "req")
        ) {
            continue;
        }
        let unit = match rest.get(1).map(String::as_str) {
            Some("per" | "/" | "a" | "an" | "every" | "each") => rest.get(2),
            _ => None,
        };
        if let Some(u) = unit.and_then(|u| unit_word(u))
            && let Ok(r) = format!("{count}/{u}").parse()
        {
            return Some(r);
        }
    }
    None
}

// ---- schemas ----

/// A tool-sized copy of a schema: local `$ref`s followed a few levels,
/// `allOf` merged, and only the keywords that help a model fill it in.
fn slim(root: &Value, schema: &Value, depth: usize) -> Value {
    let schema = deref(root, schema);
    let Some(obj) = schema.as_object() else {
        return json!({});
    };
    if depth > SCHEMA_DEPTH {
        let mut out = Map::new();
        if let Some(t) = obj.get("type").filter(|t| t.is_string()) {
            out.insert("type".into(), t.clone());
        }
        return Value::Object(out);
    }
    if let Some(all) = obj.get("allOf").and_then(Value::as_array) {
        let mut merged = json!({ "type": "object", "properties": {} });
        let mut required: Vec<Value> = Vec::new();
        for part in all {
            let part = slim(root, part, depth + 1);
            if let Some(props) = part["properties"].as_object() {
                for (k, v) in props {
                    merged["properties"][k] = v.clone();
                }
            }
            for r in part["required"].as_array().into_iter().flatten() {
                if !required.contains(r) {
                    required.push(r.clone());
                }
            }
            if let Some(d) = part.get("description") {
                merged["description"] = d.clone();
            }
        }
        if !required.is_empty() {
            merged["required"] = Value::Array(required);
        }
        return merged;
    }
    if let Some(alts) = obj
        .get("oneOf")
        .or_else(|| obj.get("anyOf"))
        .and_then(Value::as_array)
        && let Some(first) = alts.first()
    {
        let mut out = slim(root, first, depth + 1);
        if alts.len() > 1 {
            let d = out["description"].as_str().unwrap_or_default().to_owned();
            out["description"] = format!(
                "{d}{}one of {} shapes; this is the first",
                if d.is_empty() { "" } else { " — " },
                alts.len()
            )
            .into();
        }
        return out;
    }
    let mut out = Map::new();
    for (k, v) in obj {
        match k.as_str() {
            "type" => {
                // 3.1 allows a list of types; keep the first non-null.
                let t = match v {
                    Value::Array(a) => a.iter().find(|t| *t != "null").cloned(),
                    Value::String(s) if s == "file" => Some("string".into()),
                    Value::String(_) => Some(v.clone()),
                    _ => None,
                };
                if let Some(t) = t {
                    out.insert(k.clone(), t);
                }
            }
            "description" | "title" => {
                if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    out.insert(k.clone(), short(s, 200).into());
                }
            }
            "format" | "pattern" => {
                if v.is_string() {
                    out.insert(k.clone(), v.clone());
                }
            }
            "default" | "minimum" | "maximum" | "minLength" | "maxLength" | "minItems"
            | "maxItems" | "exclusiveMinimum" | "exclusiveMaximum" | "multipleOf" => {
                if !v.is_null() && !v.is_object() && !v.is_array() {
                    out.insert(k.clone(), v.clone());
                }
            }
            "enum" => {
                if let Some(a) = v.as_array() {
                    let vals: Vec<Value> = a
                        .iter()
                        .filter(|x| !x.is_null())
                        .take(50)
                        .cloned()
                        .collect();
                    if !vals.is_empty() {
                        out.insert(k.clone(), Value::Array(vals));
                    }
                }
            }
            "items" => {
                out.insert(k.clone(), slim(root, v, depth + 1));
            }
            "properties" => {
                if let Some(props) = v.as_object() {
                    let mut p = Map::new();
                    for (name, s) in props {
                        if deref(root, s)["readOnly"].as_bool() == Some(true) {
                            continue;
                        }
                        p.insert(name.clone(), slim(root, s, depth + 1));
                    }
                    out.insert(k.clone(), Value::Object(p));
                }
            }
            "additionalProperties" => {
                if v.is_boolean() {
                    out.insert(k.clone(), v.clone());
                } else if v.is_object() {
                    out.insert(k.clone(), slim(root, v, depth + 1));
                }
            }
            _ => {}
        }
    }
    // `required` only names properties that are there.
    if let (Some(req), Some(props)) = (
        obj.get("required").and_then(Value::as_array),
        out.get("properties").and_then(Value::as_object),
    ) {
        let req: Vec<Value> = req
            .iter()
            .filter(|r| r.as_str().is_some_and(|n| props.contains_key(n)))
            .cloned()
            .collect();
        if !req.is_empty() {
            out.insert("required".into(), Value::Array(req));
        }
    }
    if !out.contains_key("type") && out.contains_key("properties") {
        out.insert("type".into(), "object".into());
    }
    Value::Object(out)
}

/// The tool's params: one property per parameter, plus the body.
fn params_schema(op: &Operation) -> Value {
    let mut props = Map::new();
    let mut required = Vec::new();
    for p in &op.params {
        let mut s = p.schema.clone();
        if !s.is_object() {
            s = json!({});
        }
        let where_ = format!(
            "{} parameter{}",
            p.loc.as_str(),
            if p.arg != p.name {
                format!(" {:?}", p.name)
            } else {
                String::new()
            }
        );
        let d = s["description"].as_str().unwrap_or_default().to_owned();
        s["description"] = if d.is_empty() {
            where_
        } else {
            format!("{d} ({where_})")
        }
        .into();
        props.insert(p.arg.clone(), s);
        if p.required {
            required.push(Value::String(p.arg.clone()));
        }
    }
    if let Some(b) = &op.body {
        let mut s = b.schema.clone();
        if !s.is_object() {
            s = json!({});
        }
        if s.get("description").is_none() {
            s["description"] = "request body".into();
        }
        props.insert(b.arg.clone(), s);
        if b.required {
            required.push(Value::String(b.arg.clone()));
        }
    }
    let mut out = json!({ "type": "object", "properties": props });
    if !required.is_empty() {
        out["required"] = Value::Array(required);
    }
    out
}

// ---- YAML ----

fn from_yaml(text: &str) -> Result<Value, String> {
    use yaml_rust2::parser::{Event, Parser};
    // Aliases are expanded by the loader, so a few nested ones can make a
    // small file enormous: count what the document expands to first.
    let mut parser = Parser::new_from_str(text);
    let mut anchors: HashMap<usize, usize> = HashMap::new();
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    let mut total: usize = 0;
    loop {
        let (ev, _) = parser
            .next_token()
            .map_err(|e| format!("not valid JSON or YAML: {e}"))?;
        let add = |stack: &mut Vec<(usize, usize)>, n: usize| {
            if let Some(top) = stack.last_mut() {
                top.1 = top.1.saturating_add(n);
            }
        };
        match ev {
            Event::StreamEnd => break,
            Event::Scalar(_, _, anchor, _) => {
                if anchor > 0 {
                    anchors.insert(anchor, 1);
                }
                add(&mut stack, 1);
                total = total.saturating_add(1);
            }
            Event::Alias(id) => {
                let n = anchors.get(&id).copied().unwrap_or(1);
                add(&mut stack, n);
                total = total.saturating_add(n);
            }
            Event::SequenceStart(anchor, _) | Event::MappingStart(anchor, _) => {
                stack.push((anchor, 1));
                total = total.saturating_add(1);
            }
            Event::SequenceEnd | Event::MappingEnd => {
                if let Some((anchor, n)) = stack.pop() {
                    if anchor > 0 {
                        anchors.insert(anchor, n);
                    }
                    add(&mut stack, n);
                }
            }
            _ => {}
        }
        if total > MAX_NODES {
            return Err(format!(
                "the document expands to more than {MAX_NODES} values"
            ));
        }
    }
    let docs = yaml_rust2::YamlLoader::load_from_str(text)
        .map_err(|e| format!("not valid JSON or YAML: {e}"))?;
    let doc = docs.into_iter().next().ok_or("the document is empty")?;
    yaml_json(&doc)
}

fn yaml_json(y: &yaml_rust2::Yaml) -> Result<Value, String> {
    use yaml_rust2::Yaml;
    Ok(match y {
        Yaml::Null | Yaml::BadValue | Yaml::Alias(_) => Value::Null,
        Yaml::Boolean(b) => Value::Bool(*b),
        Yaml::Integer(i) => Value::from(*i),
        Yaml::Real(r) => r
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Value::String(r.clone()), Value::Number),
        Yaml::String(s) => Value::String(s.clone()),
        Yaml::Array(a) => Value::Array(a.iter().map(yaml_json).collect::<Result<_, _>>()?),
        Yaml::Hash(h) => {
            let mut m = Map::new();
            for (k, v) in h {
                let key = match k {
                    Yaml::String(s) | Yaml::Real(s) => s.clone(),
                    Yaml::Integer(i) => i.to_string(),
                    Yaml::Boolean(b) => b.to_string(),
                    Yaml::Null => "null".into(),
                    _ => return Err("a YAML mapping key isn't a scalar".into()),
                };
                m.insert(key, yaml_json(v)?);
            }
            Value::Object(m)
        }
    })
}

// ---- names ----

/// `listPets`, `List pets`, `pets.list` → `list_pets`.
fn snake(s: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() && prev_lower && !out.ends_with('_') {
                out.push('_');
            }
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
            out.push(c.to_ascii_lowercase());
        } else {
            prev_lower = false;
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
        }
    }
    out.trim_matches('_').to_owned()
}

/// A valid identifier from `s`: starts with a letter, at most 32 characters.
fn fit_ident(s: &str, prefix: &str) -> String {
    let mut s = s.to_owned();
    if !s.starts_with(|c: char| c.is_ascii_lowercase()) {
        s = format!("{prefix}_{s}");
    }
    tern_core::text::truncate_string(&mut s, 32);
    s.trim_end_matches('_').to_owned()
}

fn key_from_path(method: &str, path: &str) -> String {
    let mut parts = vec![method.to_owned()];
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        if let Some(p) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            parts.push(format!("by_{}", snake(p)));
        } else {
            parts.push(snake(seg));
        }
    }
    parts.retain(|p| !p.is_empty());
    parts.join("_")
}

fn unique_key(base: &str, taken: &mut BTreeSet<String>) -> String {
    let base = fit_ident(base, "op");
    let mut key = base.clone();
    let mut n = 2;
    while taken.contains(&key) || !valid_ident(&key) {
        let suffix = format!("_{n}");
        let mut b = base.clone();
        tern_core::text::truncate_string(&mut b, 32 - suffix.len());
        key = format!("{}{suffix}", b.trim_end_matches('_'));
        n += 1;
    }
    taken.insert(key.clone());
    key
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

// ---- selecting and listing ----

/// Which operations to generate. `asked` names them by tool name,
/// `operationId`, `"GET /path"` or `"tag:<tag>"`; without it, the default
/// is the first `default` supported, non-deprecated reads (or any
/// operations, if there are no reads).
pub fn select(
    doc: &Document,
    asked: Option<&[String]>,
    default: usize,
    max: usize,
) -> Result<Vec<usize>, String> {
    let ops = &doc.operations;
    let chosen: Vec<usize> = match asked {
        None | Some([]) => {
            let ok = |o: &&Operation| o.unsupported.is_none() && !o.deprecated;
            let mut v: Vec<usize> = (0..ops.len())
                .filter(|&i| ok(&&ops[i]) && ops[i].is_read())
                .collect();
            if v.is_empty() {
                v = (0..ops.len()).filter(|&i| ok(&&ops[i])).collect();
            }
            v.truncate(default);
            v
        }
        Some(names) => {
            let mut v = Vec::new();
            let mut unknown = Vec::new();
            for name in names {
                let n = name.trim();
                let hits: Vec<usize> = if let Some(tag) = n.strip_prefix("tag:") {
                    (0..ops.len())
                        .filter(|&i| {
                            ops[i]
                                .tags
                                .iter()
                                .any(|t| t.eq_ignore_ascii_case(tag.trim()))
                        })
                        .collect()
                } else {
                    (0..ops.len())
                        .filter(|&i| {
                            let o = &ops[i];
                            o.key == n
                                || o.operation_id
                                    .as_deref()
                                    .is_some_and(|id| id.eq_ignore_ascii_case(n))
                                || format!("{} {}", o.method, o.path).eq_ignore_ascii_case(n)
                        })
                        .collect()
                };
                if hits.is_empty() {
                    unknown.push(n.to_owned());
                }
                for i in hits {
                    if !v.contains(&i) {
                        v.push(i);
                    }
                }
            }
            if !unknown.is_empty() {
                return Err(format!(
                    "no operation matches {}; use the names from the listing (list: true)",
                    unknown.join(", ")
                ));
            }
            if let Some(&bad) = v.iter().find(|&&i| ops[i].unsupported.is_some()) {
                let o = &ops[bad];
                return Err(format!(
                    "{} can't be generated ({}); leave it out, or write that tool by hand",
                    o.key,
                    o.unsupported.as_deref().unwrap_or_default()
                ));
            }
            v
        }
    };
    if chosen.is_empty() {
        return Err("no operation could be generated (every one is deprecated or unsupported); pick some with `operations`".into());
    }
    if chosen.len() > max {
        return Err(format!(
            "{} operations chosen; a connector takes at most {max}. Pick fewer, or make several connectors (one per tag, say)",
            chosen.len()
        ));
    }
    Ok(chosen)
}

/// Narrowing the listing.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub tag: Option<String>,
    pub search: Option<String>,
}

/// A page of the operations, with the tags, for choosing.
pub fn listing(doc: &Document, filter: &Filter, offset: usize, limit: usize) -> String {
    let matches: Vec<&Operation> = doc
        .operations
        .iter()
        .filter(|o| {
            filter
                .tag
                .as_deref()
                .is_none_or(|t| o.tags.iter().any(|x| x.eq_ignore_ascii_case(t)))
        })
        .filter(|o| {
            filter.search.as_deref().is_none_or(|q| {
                let q = q.to_ascii_lowercase();
                [
                    o.key.as_str(),
                    o.path.as_str(),
                    o.summary.as_str(),
                    o.operation_id.as_deref().unwrap_or(""),
                ]
                .iter()
                .any(|f| f.to_ascii_lowercase().contains(&q))
            })
        })
        .collect();
    let reads = doc.operations.iter().filter(|o| o.is_read()).count();
    let mut out = format!(
        "{} {} — {} operations ({} reads, {} writes) at {}\n",
        if doc.title.is_empty() {
            "API"
        } else {
            &doc.title
        },
        if doc.version == "2.0" {
            "(Swagger 2.0)".to_owned()
        } else {
            format!("(OpenAPI {})", doc.version)
        },
        doc.operations.len(),
        reads,
        doc.operations.len() - reads,
        doc.base_url,
    );
    let tags = doc.tags();
    if tags.len() > 1 {
        let shown: Vec<String> = tags
            .iter()
            .take(30)
            .map(|(t, n)| format!("{t} ({n})"))
            .collect();
        let _ = writeln!(
            out,
            "Tags: {}{}",
            shown.join(", "),
            if tags.len() > 30 {
                format!(", and {} more", tags.len() - 30)
            } else {
                String::new()
            }
        );
    }
    let end = (offset + limit).min(matches.len());
    let what = match (&filter.tag, &filter.search) {
        (None, None) => String::new(),
        (t, s) => format!(
            " matching{}{}",
            t.as_deref()
                .map(|t| format!(" tag {t:?}"))
                .unwrap_or_default(),
            s.as_deref().map(|s| format!(" {s:?}")).unwrap_or_default()
        ),
    };
    if matches.is_empty() {
        let _ = writeln!(out, "No operations{what}.");
        return out;
    }
    let _ = writeln!(
        out,
        "Operations{what} {}–{} of {}:",
        offset.min(end) + 1,
        end,
        matches.len()
    );
    for o in matches.iter().skip(offset).take(limit) {
        let _ = writeln!(out, "- {}", o.line());
    }
    if end < matches.len() {
        let _ = writeln!(out, "(more: offset {end})");
    }
    out
}

// ---- generating ----

/// What generating produced: the Luau source and what the agent should
/// know or do next.
#[derive(Debug, Clone)]
pub struct Generated {
    pub source: String,
    /// Plain-language notes: auth, rate, anything left out.
    pub notes: Vec<String>,
    /// For an OAuth scheme: a researched plan for `auth_propose`, if
    /// `auth_discover` finds nothing.
    pub oauth_plan: Option<Value>,
    /// Secrets the user must set (key schemes).
    pub secrets: Vec<String>,
}

/// The auth a connector gets, chosen from the schemes its operations use.
enum AuthPick {
    None,
    Lua(String),
    OAuth {
        provider: String,
        scopes: Vec<String>,
        plan: Option<Value>,
    },
}

fn rank(s: &Scheme) -> u8 {
    match s {
        Scheme::OAuth2 { .. } => 0,
        Scheme::OpenIdConnect(_) => 1,
        Scheme::ApiKeyHeader(_) | Scheme::ApiKeyQuery(_) => 2,
        Scheme::Bearer | Scheme::OAuth2Token => 3,
        Scheme::Basic => 4,
        Scheme::Unsupported(_) => 9,
    }
}

fn pick_auth(
    doc: &Document,
    ops: &[&Operation],
    name: &str,
    source: &str,
    notes: &mut Vec<String>,
    secrets: &mut Vec<String>,
) -> AuthPick {
    // Schemes the chosen operations use, each with the scopes they need.
    let mut used: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for op in ops {
        for req in &op.security {
            for (scheme, scopes) in req {
                used.entry(scheme.as_str())
                    .or_default()
                    .extend(scopes.iter().cloned());
            }
        }
    }
    let best = used
        .iter()
        .filter_map(|(n, scopes)| doc.schemes.get(*n).map(|s| (*n, s, scopes)))
        .min_by_key(|(_, s, _)| rank(s));
    let Some((scheme_name, scheme, scopes)) = best else {
        if !used.is_empty() {
            notes.push(format!(
                "The operations name security schemes the document doesn't define ({}); no auth was added.",
                used.keys().copied().collect::<Vec<_>>().join(", ")
            ));
        }
        return AuthPick::None;
    };
    let upper = name.to_ascii_uppercase();
    let host = doc.host();
    let key_note = |secret: &str, how: &str| {
        format!(
            "Auth: the document's {scheme_name:?} scheme is {how}; tern sends the secret {secret} itself (the script never sees it). Check auth_discover first in case the service also offers OAuth; otherwise call secret_request for {secret} before testing."
        )
    };
    match scheme {
        Scheme::ApiKeyHeader(h) => {
            let secret = format!("{upper}_API_KEY");
            notes.push(key_note(&secret, &format!("an API key in the {h} header")));
            secrets.push(secret.clone());
            let scheme = if h.eq_ignore_ascii_case("authorization") {
                ", scheme = \"\""
            } else {
                ""
            };
            AuthPick::Lua(format!(
                "{{ secret = {}, header = {}{scheme} }}",
                lua_str(&secret),
                lua_str(h)
            ))
        }
        Scheme::ApiKeyQuery(q) => {
            let secret = format!("{upper}_API_KEY");
            notes.push(key_note(
                &secret,
                &format!("an API key in the {q} query parameter"),
            ));
            secrets.push(secret.clone());
            AuthPick::Lua(format!(
                "{{ secret = {}, query = {} }}",
                lua_str(&secret),
                lua_str(q)
            ))
        }
        Scheme::Bearer | Scheme::OAuth2Token => {
            let secret = format!("{upper}_TOKEN");
            notes.push(key_note(&secret, "a bearer token"));
            secrets.push(secret.clone());
            AuthPick::Lua(lua_str(&secret))
        }
        Scheme::Basic => {
            let (u, p) = (format!("{upper}_USER"), format!("{upper}_PASSWORD"));
            notes.push(key_note(&format!("{u} and {p}"), "HTTP Basic"));
            secrets.extend([u.clone(), p.clone()]);
            AuthPick::Lua(format!(
                "{{ basic = {{ user = {}, password = {} }} }}",
                lua_str(&u),
                lua_str(&p)
            ))
        }
        Scheme::OAuth2 {
            authorize_url,
            token_url,
            scopes: offered,
        } => {
            let builtin =
                tern_auth::profiles::find(&doc.title, Some(&host)).map(|p| p.id.to_owned());
            let provider = builtin.clone().unwrap_or_else(|| {
                let p = tern_auth::discover::provider_name(authorize_url);
                fit_ident(&p, "oauth")
            });
            let scopes: Vec<String> = scopes
                .iter()
                .filter(|s| !s.is_empty() && !s.contains(char::is_whitespace))
                .take(50)
                .cloned()
                .collect();
            let plan = builtin.is_none().then(|| {
                let mut evidence = Vec::new();
                if source.starts_with("http") {
                    evidence.push(Value::String(source.to_owned()));
                }
                json!({
                    "service": if doc.title.is_empty() { name } else { &doc.title },
                    "authorize_url": authorize_url,
                    "token_url": token_url,
                    "api_hosts": [host],
                    "scopes_supported": offered.iter().take(100).collect::<Vec<_>>(),
                    "evidence": evidence,
                })
            });
            notes.push(format!(
                "Auth: the document's {scheme_name:?} scheme is OAuth 2 (sign-in at {authorize_url}). The connector declares capabilities.oauth {provider} with the scopes its operations need, and tern sends the signed-in account's token. Signing in comes first: call auth_discover with the base URL; if it finds nothing, call auth_propose with the plan below (from the document; tern checks its addresses); then connection_add before testing."
            ));
            AuthPick::OAuth {
                provider,
                scopes,
                plan,
            }
        }
        Scheme::OpenIdConnect(url) => {
            let provider = fit_ident(&tern_auth::discover::provider_name(url), "oauth");
            notes.push(format!(
                "Auth: the document's {scheme_name:?} scheme is OpenID Connect ({url}). The connector declares capabilities.oauth {provider}; call auth_discover with that issuer's base URL, then auth_propose and connection_add before testing."
            ));
            AuthPick::OAuth {
                provider,
                scopes: scopes.iter().take(50).cloned().collect(),
                plan: None,
            }
        }
        Scheme::Unsupported(what) => {
            notes.push(format!(
                "Auth: the document's {scheme_name:?} scheme is {what}, which a connector can't send; no auth was added. Edit capabilities.auth by hand if the API offers another way."
            ));
            AuthPick::None
        }
    }
}

/// `capabilities.http` for these operations: the host with each path, `{x}`
/// segments as `*`. Too many collapse to the base path.
fn allowlist(doc: &Document, ops: &[&Operation]) -> Vec<String> {
    let host = doc.host();
    let base = doc.base_path();
    let fallback = vec![format!("{host}{base}/*")];
    let mut entries = BTreeSet::new();
    for op in ops {
        let mut path = String::new();
        let mut in_param = false;
        for c in format!("{base}{}", op.path).chars() {
            match c {
                '{' => {
                    in_param = true;
                    path.push('*');
                }
                '}' => in_param = false,
                _ if in_param => {}
                c => path.push(c),
            }
        }
        if !path.starts_with('/') {
            path.insert(0, '/');
        }
        let entry = format!("{host}{path}");
        if crate::allow::check_entry(&entry).is_err() {
            return fallback;
        }
        entries.insert(entry);
    }
    if entries.len() > MAX_ALLOW_ENTRIES {
        return fallback;
    }
    entries.into_iter().collect()
}

/// Generate the connector `name` from the operations at `chosen`.
pub fn generate(
    doc: &Document,
    name: &str,
    chosen: &[usize],
    source: &str,
) -> Result<Generated, String> {
    if !valid_ident(name) {
        return Err(format!(
            "name {name:?} must match [a-z][a-z0-9_]* (max 32 chars)"
        ));
    }
    let ops: Vec<&Operation> = chosen
        .iter()
        .filter_map(|&i| doc.operations.get(i))
        .collect();
    let mut notes = Vec::new();
    let mut secrets = Vec::new();
    let auth = pick_auth(doc, &ops, name, source, &mut notes, &mut secrets);
    let http = allowlist(doc, &ops);

    let mut s = String::new();
    let title = if doc.title.is_empty() {
        "an API".to_owned()
    } else {
        doc.title.clone()
    };
    let _ = writeln!(
        s,
        "-- Generated by tern from the OpenAPI document of {}.",
        one_line(&title)
    );
    if !source.is_empty() {
        let _ = writeln!(s, "-- Source: {}", one_line(source));
    }
    let _ = writeln!(
        s,
        "-- {} of {} operations. Regenerate with connector_from_openapi to change the set.",
        ops.len(),
        doc.operations.len()
    );
    let _ = writeln!(s, "local BASE = {}", lua_str(&doc.base_url));
    s.push_str(HELPERS);
    s.push_str("\nlocal OPS = {\n");
    for op in &ops {
        let _ = write!(
            s,
            "  {} = {{ method = {}, path = {}, params = {{",
            op.key,
            lua_str(&op.method),
            lua_str(&op.path)
        );
        for p in &op.params {
            let _ = write!(
                s,
                " {{ name = {}, arg = {}, at = {}, required = {}, explode = {} }},",
                lua_str(&p.name),
                lua_str(&p.arg),
                lua_str(p.loc.as_str()),
                p.required,
                p.explode
            );
        }
        s.push_str(" }");
        if let Some(b) = &op.body {
            let _ = write!(
                s,
                ", body = {}, form = {}, body_required = {}",
                lua_str(&b.arg),
                b.form,
                b.required
            );
        }
        s.push_str(" },\n");
    }
    s.push_str("}\n\nreturn {\n");
    let _ = writeln!(s, "  name = {},", lua_str(name));
    let mut description = if doc.title.is_empty() {
        format!("{} API", doc.host())
    } else {
        doc.title.clone()
    };
    if !doc.description.is_empty() {
        description = format!(
            "{description}: {}",
            short(doc.description.lines().next().unwrap_or_default(), 160)
        );
    }
    let _ = writeln!(s, "  description = {},", lua_str(&description));
    s.push_str("  capabilities = {\n    http = {");
    for h in &http {
        let _ = write!(s, " {},", lua_str(h));
    }
    s.push_str(" },\n");
    let mut oauth_plan = None;
    match auth {
        AuthPick::None => {}
        AuthPick::Lua(a) => {
            let _ = writeln!(s, "    auth = {a},");
        }
        AuthPick::OAuth {
            provider,
            scopes,
            plan,
        } => {
            let list: Vec<String> = scopes.iter().map(|x| lua_str(x)).collect();
            let _ = writeln!(
                s,
                "    oauth = {{ {provider} = {{ scopes = {{ {} }} }} }},",
                list.join(", ")
            );
            oauth_plan = plan;
        }
    }
    if let Some(r) = doc.rate {
        let _ = writeln!(s, "    rate = {},", lua_str(&r.to_string()));
        notes.push(format!(
            "Rate: the document states {r}; the connector declares it."
        ));
    }
    s.push_str("  },\n  tools = {\n");
    for op in &ops {
        let mut desc = if op.summary.is_empty() {
            op.key.replace('_', " ")
        } else {
            op.summary.clone()
        };
        desc = format!("{} ({} {})", short(&desc, 200), op.method, op.path);
        if op.deprecated {
            desc.push_str(" [deprecated]");
        }
        let _ = writeln!(s, "    {} = {{", op.key);
        let _ = writeln!(s, "      description = {},", lua_str(&desc));
        let _ = writeln!(s, "      effect = {},", lua_str(op.effect()));
        if op.is_read() {
            s.push_str("      untrusted = true,\n");
        }
        if matches!(op.method.as_str(), "GET" | "HEAD" | "PUT" | "DELETE") {
            s.push_str("      idempotent = true,\n");
        }
        let _ = writeln!(s, "      params = {},", lua_value(&params_schema(op), 3));
        let _ = writeln!(
            s,
            "      run = function(ctx, args) return call(ctx, OPS.{}, args) end,",
            op.key
        );
        s.push_str("    },\n");
    }
    s.push_str("  },\n}\n");
    if ops.iter().any(|o| o.is_read()) {
        notes.push("Read tools are marked untrusted (their results are the API's data, not the user's words); remove `untrusted = true` from a tool whose results are only the user's own data.".into());
    }
    Ok(Generated {
        source: s,
        notes,
        oauth_plan,
        secrets,
    })
}

/// The shared request code every generated tool calls.
const HELPERS: &str = r#"
local function enc(s)
  return (string.gsub(s, "[^%w%-%._~]", function(c)
    return string.format("%%%02X", string.byte(c))
  end))
end

local function text(v)
  if type(v) == "boolean" then return v and "true" or "false" end
  if type(v) == "number" and v == math.floor(v) and math.abs(v) < 2^53 then
    return string.format("%d", v)
  end
  if type(v) == "table" then
    local parts = {}
    for _, x in ipairs(v) do table.insert(parts, text(x)) end
    return table.concat(parts, ",")
  end
  return tostring(v)
end

local function replace(s, find, with)
  local i, j = string.find(s, find, 1, true)
  if not i then return s end
  return string.sub(s, 1, i - 1) .. with .. string.sub(s, j + 1)
end

local function call(ctx, op, args)
  args = args or {}
  local path, query = op.path, {}
  local headers = { Accept = "application/json" }
  for _, p in ipairs(op.params) do
    local v = args[p.arg]
    if v == nil then
      if p.required then error("missing required argument " .. p.arg) end
    elseif p.at == "path" then
      path = replace(path, "{" .. p.name .. "}", enc(text(v)))
    elseif p.at == "query" then
      if type(v) == "table" and p.explode then
        for _, x in ipairs(v) do table.insert(query, enc(p.name) .. "=" .. enc(text(x))) end
      else
        table.insert(query, enc(p.name) .. "=" .. enc(text(v)))
      end
    else
      headers[p.name] = text(v)
    end
  end
  local url = BASE .. path
  if #query > 0 then url = url .. "?" .. table.concat(query, "&") end
  local req = { method = op.method, url = url, headers = headers }
  if op.body then
    local b = args[op.body]
    if b == nil then
      if op.body_required then error("missing required argument " .. op.body) end
    elseif op.form then
      local keys, parts = {}, {}
      for k in pairs(b) do table.insert(keys, k) end
      table.sort(keys)
      for _, k in ipairs(keys) do table.insert(parts, enc(k) .. "=" .. enc(text(b[k]))) end
      req.body = table.concat(parts, "&")
      headers["Content-Type"] = "application/x-www-form-urlencoded"
    else
      req.json = b
    end
  end
  local res = ctx.http.request(req)
  if not res.ok then
    error(op.method .. " " .. op.path .. " returned HTTP " .. res.status .. ": " .. string.sub(res.body or "", 1, 500))
  end
  if res.json ~= nil then return res.json end
  if res.body == nil or res.body == "" then return { status = res.status } end
  return res.body
end
"#;

fn one_line(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

/// A Luau string literal.
pub fn lua_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\{:03}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while", "continue",
];

fn lua_key(k: &str) -> String {
    let ident = k.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !LUA_KEYWORDS.contains(&k);
    if ident {
        k.to_owned()
    } else {
        format!("[{}]", lua_str(k))
    }
}

/// A JSON value as a Luau table literal. Empty arrays never appear (the
/// slimmed schemas drop them), so `{}` is always an object.
fn lua_value(v: &Value, indent: usize) -> String {
    match v {
        Value::Null => "nil".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => lua_str(s),
        Value::Array(a) => {
            let items: Vec<String> = a.iter().map(|x| lua_value(x, indent + 1)).collect();
            format!("{{ {} }}", items.join(", "))
        }
        Value::Object(o) if o.is_empty() => "{}".into(),
        Value::Object(o) => {
            let pad = "  ".repeat(indent + 1);
            let mut out = "{\n".to_owned();
            for (k, x) in o {
                if x.is_null() {
                    continue;
                }
                let _ = writeln!(out, "{pad}{} = {},", lua_key(k), lua_value(x, indent + 1));
            }
            out.push_str(&"  ".repeat(indent));
            out.push('}');
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(snake("listPets"), "list_pets");
        assert_eq!(snake("Get pet by ID"), "get_pet_by_id");
        assert_eq!(snake("pets.list"), "pets_list");
        assert_eq!(snake("HTTPStatus"), "httpstatus");
        assert_eq!(key_from_path("get", "/pets/{petId}"), "get_pets_by_pet_id");
        let mut taken = BTreeSet::new();
        assert_eq!(unique_key("list", &mut taken), "list");
        assert_eq!(unique_key("list", &mut taken), "list_2");
        assert_eq!(unique_key("9lives", &mut taken), "op_9lives");
        let long = "a".repeat(40);
        let k = unique_key(&long, &mut taken);
        assert!(valid_ident(&k), "{k}");
        let k2 = unique_key(&long, &mut taken);
        assert!(valid_ident(&k2) && k2 != k, "{k2}");
    }

    #[test]
    fn rates_from_text_and_extensions() {
        let r = |s: &str| rate_in_text(s).map(|r| r.to_string());
        assert_eq!(
            r("Limited to 100 requests per minute."),
            Some("100/min".into())
        );
        assert_eq!(r("up to 5 calls/second"), Some("5/sec".into()));
        assert_eq!(r("1000 requests an hour"), Some("1000/hour".into()));
        assert_eq!(r("version 2 of the API"), None);
        let v = |j: Value| rate_value(&j).map(|r| r.to_string());
        assert_eq!(v(json!("60/min")), Some("60/min".into()));
        assert_eq!(v(json!(30)), Some("30/min".into()));
        assert_eq!(
            v(json!({"limit": 10, "period": "second"})),
            Some("10/sec".into())
        );
        assert_eq!(
            v(json!({"requests": 500, "window": 3600})),
            Some("500/hour".into())
        );
    }

    #[test]
    fn lua_literals() {
        assert_eq!(lua_str("a\"b\\c\nd\u{1}"), "\"a\\\"b\\\\c\\nd\\001\"");
        assert_eq!(lua_key("end"), "[\"end\"]");
        assert_eq!(lua_key("pet-id"), "[\"pet-id\"]");
        assert_eq!(lua_key("limit"), "limit");
    }

    #[test]
    fn yaml_bombs_are_refused() {
        let mut bomb = String::from(
            "a: &a [\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\"]\n",
        );
        let mut prev = "a".to_owned();
        for n in ["b", "c", "d", "e", "f", "g", "h", "i"] {
            bomb.push_str(&format!(
                "{n}: &{n} [*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev}]\n"
            ));
            prev = n.to_owned();
        }
        let err = from_yaml(&bomb).unwrap_err();
        assert!(err.contains("expands to more than"), "{err}");
        // Ordinary aliases are fine.
        let ok = from_yaml("a: &x {k: 1}\nb: *x\n").unwrap();
        assert_eq!(ok["b"]["k"], 1);
    }
}
