//! Quarantined fetch (docs/quarantined-fetch-design.md): every web page is
//! read by a separate low-privilege reader, never by the calling agent.
//!
//! * [`ingest`] is the one choke point where an ingestion tool's output is
//!   given its trust level (F0).
//! * [`FetchGateway`] is what `http_fetch` calls (F1): it validates the
//!   brief, applies domain policy, budgets and the cache, fetches under the
//!   SSRF checks of [`crate::net`], runs the no-model [`extract`] path or the
//!   model-backed reader ([`reader`]), validates the result in Rust
//!   ([`schema`]) and returns it with the right [`Trust`].

pub mod extract;
pub mod reader;
pub mod sanitize;
pub mod schema;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tern_store::{LedgerEntry, Store};

use crate::config::{FetchConfig, FetchTaint, ModelChain};
use crate::net::get_public_with;
use crate::tool::{Ingestion, ToolCtx, ToolResult, Trust};
use crate::types::Provider;

use sanitize::{clean_line, cut};
use schema::{FetchResult, Source, Structured};

/// F0, the single gateway: give an ingestion tool's result the trust its
/// kind demands (never lowering one the tool already set). Tools call this
/// instead of setting a taint flag themselves; the agent loop applies the
/// same floor from [`crate::tool::Tool::ingestion`] as a backstop.
pub fn ingest(kind: Ingestion, result: ToolResult) -> ToolResult {
    result.with_trust(kind.floor())
}

/// A model that can read a page, with what it costs.
pub struct FetcherModel {
    pub provider: Arc<dyn Provider>,
    /// The model name the provider is asked for.
    pub model: String,
    /// `provider:model`, for the ledger.
    pub label: String,
    /// US dollars per million input and output tokens, when known.
    pub price: Option<(f64, f64)>,
}

/// Picks the reader's model: the configured `[fetch] model`, or the cheapest
/// model the user already has configured. Implemented by the binary, which
/// owns providers and prices.
pub trait FetcherModels: Send + Sync {
    fn pick(&self, configured: Option<&ModelChain>) -> Result<FetcherModel, String>;
}

/// What a caller may ask of the reader.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Want {
    pub structured: bool,
    pub text: bool,
    pub facts: bool,
    pub answer: bool,
    pub links: bool,
}

impl Want {
    fn parse(names: &[String]) -> Result<Self, String> {
        let mut w = Want::default();
        for n in names {
            match n.as_str() {
                "structured" => w.structured = true,
                "text" => w.text = true,
                "facts" => w.facts = true,
                "answer" => w.answer = true,
                "links" => w.links = true,
                other => {
                    return Err(format!(
                        "unknown want {other:?} (structured, text, facts, answer, links)"
                    ));
                }
            }
        }
        Ok(w)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Extract,
    Ask,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Extract => "extract",
            Mode::Ask => "ask",
        }
    }
}

/// A validated brief. There is no field for instructions: the reader's
/// system prompt is compiled in.
#[derive(Debug, Clone)]
pub struct Brief {
    pub url: reqwest::Url,
    pub question: Option<String>,
    pub selectors: Vec<String>,
    pub want: Want,
    pub mode: Mode,
    pub max_bytes: Option<usize>,
    /// Notes tern adds to the result (an ignored argument).
    pub notes: Vec<String>,
}

pub const MAX_QUESTION: usize = 500;
const MAX_SELECTORS: usize = 8;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBrief {
    url: String,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    selectors: Vec<String>,
    #[serde(default)]
    want: Option<Vec<String>>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    max_bytes: Option<usize>,
}

/// Validate the caller's arguments.
pub fn parse_brief(raw: Value) -> Result<Brief, String> {
    let b: RawBrief = serde_json::from_value(raw).map_err(|e| format!("invalid arguments: {e}"))?;
    let url = reqwest::Url::parse(b.url.trim()).map_err(|e| format!("invalid url: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("unsupported scheme {:?}", url.scheme()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("urls with credentials are refused".into());
    }
    let question = b
        .question
        .map(|q| q.trim().to_owned())
        .filter(|q| !q.is_empty());
    if question
        .as_ref()
        .is_some_and(|q| q.chars().count() > MAX_QUESTION)
    {
        return Err(format!("question is over {MAX_QUESTION} characters"));
    }
    if b.selectors.len() > MAX_SELECTORS || b.selectors.iter().any(|s| s.len() > 100) {
        return Err(format!(
            "at most {MAX_SELECTORS} selectors of 100 characters"
        ));
    }
    let mut notes = Vec::new();
    let mode = match (b.mode.as_deref(), &question) {
        (Some("extract"), q) => {
            if q.is_some() {
                notes.push("question ignored: mode is extract".to_owned());
            }
            Mode::Extract
        }
        (Some("ask"), Some(_)) | (None, Some(_)) => Mode::Ask,
        (Some("ask"), None) => return Err("mode ask needs a question".into()),
        (None, None) => Mode::Extract,
        (Some(other), _) => return Err(format!("unknown mode {other:?} (extract or ask)")),
    };
    let want = match &b.want {
        Some(names) => Want::parse(names)?,
        None => match mode {
            // Old prompts (a url only) keep working: structure, links and a
            // text excerpt, tainting as today.
            Mode::Extract => Want {
                structured: true,
                text: true,
                links: true,
                ..Want::default()
            },
            Mode::Ask => Want {
                facts: true,
                answer: true,
                links: true,
                ..Want::default()
            },
        },
    };
    Ok(Brief {
        url,
        question: question.filter(|_| mode == Mode::Ask),
        selectors: b.selectors,
        want,
        mode,
        max_bytes: b.max_bytes,
        notes,
    })
}

/// A site matches a domain list entry itself or as a subdomain.
pub fn domain_listed(host: &str, list: &[String]) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    list.iter().any(|d| {
        let d = d
            .trim()
            .trim_start_matches("*.")
            .trim_end_matches('.')
            .to_ascii_lowercase();
        !d.is_empty() && (host == d || host.ends_with(&format!(".{d}")))
    })
}

/// Deny wins; an empty allow list means any public site.
pub fn domain_allowed(host: &str, cfg: &FetchConfig) -> Result<(), String> {
    if domain_listed(host, &cfg.deny_domains) {
        return Err(format!("{host} is on the fetch deny list"));
    }
    if !cfg.allow_domains.is_empty() && !domain_listed(host, &cfg.allow_domains) {
        return Err(format!("{host} is not on the fetch allow list"));
    }
    Ok(())
}

struct CacheEntry {
    at: Instant,
    result: FetchResult,
}

/// The gateway `http_fetch` calls. One per process.
pub struct FetchGateway {
    settings: Arc<dyn Fn() -> FetchConfig + Send + Sync>,
    store: Option<Store>,
    models: OnceLock<Arc<dyn FetcherModels>>,
    audit_dir: Option<PathBuf>,
    allow_loopback: bool,
    cache: Mutex<HashMap<String, CacheEntry>>,
    calls: Mutex<HashMap<String, u32>>,
    inflight: AtomicUsize,
}

impl FetchGateway {
    pub fn new(settings: Arc<dyn Fn() -> FetchConfig + Send + Sync>, store: Option<Store>) -> Self {
        Self {
            settings,
            store,
            models: OnceLock::new(),
            audit_dir: None,
            allow_loopback: false,
            cache: Mutex::new(HashMap::new()),
            calls: Mutex::new(HashMap::new()),
            inflight: AtomicUsize::new(0),
        }
    }

    /// Fixed settings, for tests and tools built without a config.
    pub fn with_config(cfg: FetchConfig, store: Option<Store>) -> Self {
        Self::new(Arc::new(move || cfg.clone()), store)
    }

    /// Keep the raw page of each fetch here, for audit (hash in the ledger).
    pub fn audit_dir(mut self, dir: PathBuf) -> Self {
        self.audit_dir = Some(dir);
        self
    }

    /// Tests fetch from a local mock server; only loopback is let through.
    pub fn allow_loopback(mut self) -> Self {
        self.allow_loopback = true;
        self
    }

    /// Where the reader's model comes from. Set once, after the providers
    /// exist; until then `ask` degrades to `extract`.
    pub fn set_models(&self, models: Arc<dyn FetcherModels>) {
        let _ = self.models.set(models);
    }

    pub fn config(&self) -> FetchConfig {
        (self.settings)()
    }

    /// Whether the caller's run is tainted (it read free text or started
    /// from untrusted input).
    fn caller_tainted(&self, ctx: &ToolCtx) -> bool {
        self.store
            .as_ref()
            .and_then(|s| s.run(&ctx.run_id).ok().flatten())
            .is_some_and(|r| r.taint.is_some() || r.input_taint.is_some())
    }

    /// Run a fetch for `ctx`'s run. Never returns page text outside the
    /// validated result.
    pub async fn fetch(&self, ctx: &ToolCtx, raw: Value) -> ToolResult {
        let cfg = self.config();
        let mut brief = match parse_brief(raw) {
            Ok(b) => b,
            Err(e) => return ToolResult::error(e),
        };
        let host = brief
            .url
            .host_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if let Err(e) = domain_allowed(&host, &cfg) {
            return ToolResult::error(e);
        }
        // A tainted caller's free-text question is never sent to a model.
        if brief.mode == Mode::Ask && !cfg.ask_when_tainted && self.caller_tainted(ctx) {
            brief.mode = Mode::Extract;
            brief.question = None;
            brief.want.facts = false;
            brief.want.answer = false;
            brief.want.structured |= !brief.want.text && !brief.want.links;
            brief.notes.push(
                "forced to extract: this run read untrusted content, so it can't ask a question"
                    .to_owned(),
            );
        }
        let calls = {
            let mut m = self.calls.lock().unwrap_or_else(|p| p.into_inner());
            let n = m.entry(ctx.run_id.clone()).or_insert(0);
            *n += 1;
            *n
        };
        if calls > cfg.max_calls_per_run {
            return ToolResult::error(format!(
                "this run has made {} fetches (the limit); stop fetching",
                cfg.max_calls_per_run
            ));
        }
        if self.inflight.fetch_add(1, Ordering::SeqCst) >= cfg.max_concurrent.max(1) {
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            return ToolResult::error("too many fetches in flight; try again in a moment");
        }
        let out = self.run(ctx, &cfg, brief).await;
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        out
    }

    fn cache_key(brief: &Brief, max: usize) -> String {
        let key = json!([
            brief.url.as_str(),
            brief.mode.as_str(),
            brief.question,
            brief.selectors,
            [
                brief.want.structured,
                brief.want.text,
                brief.want.facts,
                brief.want.answer,
                brief.want.links
            ],
            max
        ]);
        blake3::hash(key.to_string().as_bytes())
            .to_hex()
            .to_string()
    }

    async fn run(&self, ctx: &ToolCtx, cfg: &FetchConfig, brief: Brief) -> ToolResult {
        let host = brief
            .url
            .host_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let max = brief
            .max_bytes
            .unwrap_or(cfg.max_bytes)
            .clamp(1, cfg.max_bytes.max(1));
        let key = Self::cache_key(&brief, max);
        self.ledger(
            ctx,
            "fetch.request",
            &host,
            format!("fetch {} ({})", brief.url, brief.mode.as_str()),
            json!({ "url": brief.url.as_str(), "mode": brief.mode.as_str(),
                    "question": brief.question, "selectors": brief.selectors }),
        );
        if cfg.cache_secs > 0 {
            let hit = {
                let c = self.cache.lock().unwrap_or_else(|p| p.into_inner());
                c.get(&key)
                    .filter(|e| e.at.elapsed() < Duration::from_secs(cfg.cache_secs))
                    .map(|e| e.result.clone())
            };
            if let Some(result) = hit {
                self.ledger(
                    ctx,
                    "fetch.result",
                    &host,
                    format!("fetched {host} (cache hit)"),
                    json!({ "cached": true, "cost_usd": 0.0, "trust": trust_of(&result, cfg).as_str() }),
                );
                return finish(result, cfg);
            }
        }
        let deadline = Duration::from_secs(cfg.timeout_secs.max(1)) * (cfg.max_pages as u32 + 2);
        let fut = self.read(ctx, cfg, &brief, max);
        let outcome = tokio::select! {
            r = tokio::time::timeout(deadline, fut) => match r {
                Ok(r) => r,
                Err(_) => Err("the fetch took too long".to_owned()),
            },
            () = ctx.cancel.cancelled() => Err("cancelled".to_owned()),
        };
        match outcome {
            Ok(read) => {
                let mut result = read.result;
                result.notes = join_notes(brief.notes.iter().cloned().chain(result.notes.take()));
                self.ledger(
                    ctx,
                    "fetch.result",
                    &host,
                    format!(
                        "fetched {host}: {} bytes, {} facts",
                        result.source.bytes,
                        result.facts.len()
                    ),
                    json!({
                        "cached": false,
                        "page_hash": read.page_hash,
                        "page_file": read.page_file,
                        "bytes": result.source.bytes,
                        "trust": trust_of(&result, cfg).as_str(),
                        "dropped": read.dropped,
                        "cost_usd": read.cost_usd,
                        "model": read.model,
                        "status": result.source.status,
                    }),
                );
                if cfg.cache_secs > 0 {
                    let mut c = self.cache.lock().unwrap_or_else(|p| p.into_inner());
                    c.retain(|_, e| e.at.elapsed() < Duration::from_secs(cfg.cache_secs));
                    if c.len() < 256 {
                        c.insert(
                            key,
                            CacheEntry {
                                at: Instant::now(),
                                result: result.clone(),
                            },
                        );
                    }
                }
                finish(result, cfg)
            }
            Err(e) => {
                let e = clean_line(&e, 300);
                self.ledger(
                    ctx,
                    "fetch.result",
                    &host,
                    format!("fetch of {host} failed: {e}"),
                    json!({ "error": e }),
                );
                ToolResult::error(format!("fetch failed: {e}"))
            }
        }
    }

    fn ledger(&self, ctx: &ToolCtx, kind: &str, host: &str, summary: String, detail: Value) {
        let Some(store) = &self.store else { return };
        let actor = store
            .run_agent(&ctx.run_id)
            .map(|(a, _)| format!("agent:{a}"))
            .unwrap_or_else(|_| "tern".into());
        let e = LedgerEntry::new(kind, host, summary)
            .actor(&actor)
            .run(&ctx.run_id, &ctx.session_id)
            .detail(detail);
        if let Err(e) = store.ledger_append(&e) {
            tracing::warn!(error = %e, "ledger write for a fetch failed");
        }
    }

    /// Fetch the page and read it: extract or ask.
    async fn read(
        &self,
        ctx: &ToolCtx,
        cfg: &FetchConfig,
        brief: &Brief,
        max: usize,
    ) -> Result<Read, String> {
        let page = fetch_page(
            brief.url.as_str(),
            max,
            cfg,
            self.allow_loopback,
            &ctx.run_id,
        )
        .await?;
        let page_file = self.save_audit(ctx, &page);
        let page_hash = page.hash.clone();
        let mut result = page.result_base(brief.mode);
        let mut read = Read {
            result: FetchResult {
                notes: None,
                ..result.clone()
            },
            page_hash,
            page_file,
            dropped: schema::Dropped::default(),
            cost_usd: 0.0,
            model: None,
        };
        if brief.want.structured || !brief.selectors.is_empty() {
            result.structured = Some(page.structured(&brief.selectors));
        }
        if brief.want.links {
            result.links = page.links();
        }
        match brief.mode {
            Mode::Extract => {
                if brief.want.text {
                    result.text = Some(cut(&page.text(), cfg.extract_text_bytes).to_owned())
                        .filter(|t| !t.is_empty());
                }
            }
            Mode::Ask => {
                let question = brief.question.as_deref().unwrap_or_default();
                match self.models.get().map(|m| m.pick(cfg.model.as_ref())) {
                    Some(Ok(model)) => {
                        let out = reader::ask(
                            ctx,
                            cfg,
                            &model,
                            question,
                            brief,
                            page,
                            self.allow_loopback,
                        )
                        .await?;
                        if brief.want.facts {
                            result.facts = out.validated.facts;
                        }
                        if brief.want.links {
                            result.links = out.validated.links;
                        }
                        result.answer = out.validated.answer;
                        result.confidence = out.validated.confidence;
                        result.notes = out.validated.notes;
                        result.source = out.source;
                        result.truncated |= out.truncated;
                        read.dropped = out.validated.dropped;
                        read.cost_usd = out.cost_usd;
                        read.model = Some(model.label);
                    }
                    Some(Err(why)) => {
                        result.mode = "extract".into();
                        result.notes = Some(format!(
                            "no reader model ({}); returned the page's own structure instead",
                            clean_line(&why, 120)
                        ));
                        result.text = Some(cut(&page.text(), cfg.extract_text_bytes).to_owned())
                            .filter(|t| !t.is_empty() && brief.want.answer);
                        result
                            .structured
                            .get_or_insert_with(|| page.structured(&[]));
                    }
                    None => {
                        result.mode = "extract".into();
                        result.notes = Some(
                            "no reader model is available; returned the page's own structure instead"
                                .to_owned(),
                        );
                        result.text = Some(cut(&page.text(), cfg.extract_text_bytes).to_owned())
                            .filter(|t| !t.is_empty() && brief.want.answer);
                        result
                            .structured
                            .get_or_insert_with(|| page.structured(&[]));
                    }
                }
            }
        }
        result.fit(cfg.result_max_bytes);
        read.result = result;
        Ok(read)
    }

    /// Keep the raw page (capped) for audit; returns the file name.
    fn save_audit(&self, ctx: &ToolCtx, page: &Page) -> Option<String> {
        let dir = self.audit_dir.as_ref()?;
        std::fs::create_dir_all(dir).ok()?;
        let name = format!("{}-{}.txt", ctx.run_id, &page.hash[..16]);
        let body = String::from_utf8_lossy(&page.raw);
        std::fs::write(dir.join(&name), cut(&body, 256 * 1024)).ok()?;
        Some(name)
    }
}

struct Read {
    result: FetchResult,
    page_hash: String,
    page_file: Option<String>,
    dropped: schema::Dropped,
    cost_usd: f64,
    model: Option<String>,
}

fn join_notes(notes: impl Iterator<Item = String>) -> Option<String> {
    let all: Vec<String> = notes.filter(|n| !n.is_empty()).collect();
    (!all.is_empty()).then(|| all.join("; "))
}

/// The trust a result carries: `Data` when it holds only typed, verified
/// fields (and taint is `strict`); `Untrusted` when it holds free text, or
/// always under `legacy`.
pub fn trust_of(result: &FetchResult, cfg: &FetchConfig) -> Trust {
    if cfg.taint == FetchTaint::Legacy || result.has_free_text() {
        Trust::Untrusted
    } else {
        Trust::Data
    }
}

fn finish(result: FetchResult, cfg: &FetchConfig) -> ToolResult {
    let trust = trust_of(&result, cfg);
    let content = serde_json::to_string(&result).unwrap_or_else(|e| e.to_string());
    ToolResult::ok(content).with_trust(trust)
}

/// A fetched, parsed page: the reader's `fetch_page` tool is this.
pub struct Page {
    pub source: Source,
    pub truncated: bool,
    pub raw: Vec<u8>,
    pub hash: String,
    pub parsed: extract::Parsed,
    plain: Option<String>,
}

impl Page {
    pub fn text(&self) -> String {
        self.plain
            .clone()
            .unwrap_or_else(|| self.parsed.text.clone())
    }

    fn title(&self) -> String {
        if self.parsed.title.is_empty() {
            self.source.host.clone()
        } else {
            self.parsed.title.clone()
        }
    }

    fn result_base(&self, mode: Mode) -> FetchResult {
        FetchResult {
            source: self.source.clone(),
            truncated: self.truncated,
            mode: mode.as_str().to_owned(),
            title: self.title(),
            structured: None,
            facts: Vec::new(),
            answer: None,
            text: None,
            links: Vec::new(),
            confidence: None,
            notes: None,
        }
    }

    fn structured(&self, selectors: &[String]) -> Structured {
        let mut s = Structured {
            meta: self.parsed.meta.clone(),
            headings: self.parsed.headings.clone(),
            tables: self.parsed.tables.clone(),
            ..Structured::default()
        };
        for sel in selectors {
            s.selectors
                .insert(clean_line(sel, 100), self.parsed.select(sel, 10));
        }
        s
    }

    pub fn links(&self) -> Vec<schema::Link> {
        self.parsed
            .anchors
            .iter()
            .filter(|(u, _)| u.len() <= schema::MAX_URL)
            .take(schema::MAX_LINKS)
            .map(|(u, t)| schema::Link {
                url: u.clone(),
                text: t.clone(),
            })
            .collect()
    }
}

/// `fetch_page`: GET `url` under the SSRF checks, redirects checked per hop,
/// no cookies, no credentials, and parse what comes back. Errors never carry
/// page text.
pub async fn fetch_page(
    url: &str,
    max: usize,
    cfg: &FetchConfig,
    allow_loopback: bool,
    _run: &str,
) -> Result<Page, String> {
    let f = get_public_with(
        url,
        max,
        allow_loopback,
        Duration::from_secs(cfg.timeout_secs.max(1)),
        cfg.max_redirects,
    )
    .await?;
    let final_url = f.final_url.clone();
    let host = reqwest::Url::parse(&final_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    // A redirect may land on a denied site.
    domain_allowed(&host, cfg)?;
    if !f.status.is_success() {
        return Err(format!("HTTP {}", f.status));
    }
    let ct = f
        .content_type
        .clone()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let is_html = ct.contains("html") || ct.is_empty();
    let is_text = ct.starts_with("text/") || ct.contains("json") || ct.contains("xml");
    if !is_html && !is_text {
        return Err(format!("unsupported content type {}", clean_line(&ct, 60)));
    }
    let source = Source {
        url: cut(&final_url, schema::MAX_URL).to_owned(),
        host,
        status: f.status.as_u16(),
        fetched_at: chrono::Utc::now().to_rfc3339(),
        content_type: f.content_type.as_deref().map(|c| clean_line(c, 80)),
        bytes: f.body.len(),
    };
    let hash = blake3::hash(&f.body).to_hex().to_string();
    let (parsed, plain) = if is_html {
        let html = String::from_utf8_lossy(&f.body);
        (extract::parse_html(&html, &final_url), None)
    } else {
        (
            extract::Parsed::default(),
            Some(extract::plain_text(&f.body)),
        )
    };
    Ok(Page {
        source,
        truncated: f.truncated,
        raw: f.body,
        hash,
        parsed,
        plain,
    })
}

#[cfg(test)]
mod fuzz;
#[cfg(test)]
pub(crate) mod tests;
