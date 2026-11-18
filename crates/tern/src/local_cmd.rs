//! `tern local`: models on this Mac (docs/local-models.md). Through the
//! running daemon when there is one, so its window sees a download; else
//! straight to Ollama.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use serde_json::{Value, json};
use tern_core::TernHome;

use crate::cli::LocalCommand;
use crate::local::Local;

/// Where the commands go.
enum Target {
    Daemon(reqwest::Client),
    Direct(Arc<Local>),
}

impl Target {
    fn open(home: &TernHome) -> Self {
        match crate::lifecycle::socket_client(home) {
            Some(c) => Self::Daemon(c),
            None => {
                let (tx, _) = tokio::sync::broadcast::channel(16);
                Self::Direct(Arc::new(Local::system(tx)))
            }
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<Value> {
        let Self::Daemon(c) = self else {
            unreachable!("only the daemon is called over HTTP")
        };
        let mut req = c
            .request(method, format!("http://tern{path}"))
            // A download isn't short, but each call is.
            .timeout(Duration::from_secs(30));
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.context("couldn't reach the tern daemon")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_owned))
                .unwrap_or(text);
            bail!("{msg}");
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn status(&self) -> anyhow::Result<Value> {
        match self {
            Self::Daemon(_) => {
                self.call(reqwest::Method::GET, "/v1/local/status", None)
                    .await
            }
            Self::Direct(l) => Ok(serde_json::to_value(l.status().await)?),
        }
    }

    async fn models(&self) -> anyhow::Result<Value> {
        match self {
            Self::Daemon(_) => {
                self.call(reqwest::Method::GET, "/v1/local/models", None)
                    .await
            }
            Self::Direct(l) => Ok(serde_json::to_value(
                l.models().await.map_err(anyhow::Error::msg)?,
            )?),
        }
    }

    async fn pull(&self, model: &str) -> anyhow::Result<Value> {
        match self {
            Self::Daemon(_) => {
                self.call(
                    reqwest::Method::POST,
                    "/v1/local/pull",
                    Some(json!({ "model": model })),
                )
                .await
            }
            Self::Direct(l) => Ok(serde_json::to_value(
                l.pull(model).await.map_err(anyhow::Error::msg)?,
            )?),
        }
    }

    async fn pulls(&self) -> anyhow::Result<Value> {
        match self {
            Self::Daemon(_) => {
                self.call(reqwest::Method::GET, "/v1/local/pulls", None)
                    .await
            }
            Self::Direct(l) => Ok(serde_json::to_value(l.pulls())?),
        }
    }

    async fn cancel(&self, id: &str) {
        match self {
            Self::Daemon(_) => {
                let _ = self
                    .call(
                        reqwest::Method::POST,
                        &format!("/v1/local/pulls/{id}/cancel"),
                        None,
                    )
                    .await;
            }
            Self::Direct(l) => {
                let _ = l.cancel_pull(id);
            }
        }
    }
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
}

pub fn run(home: &TernHome, command: Option<LocalCommand>) -> anyhow::Result<ExitCode> {
    let target = Target::open(home);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        match command.unwrap_or(LocalCommand::Status { json: false }) {
            LocalCommand::Status { json } => status(&target, json).await,
            LocalCommand::List { json } => list(&target, json).await,
            LocalCommand::Pull { model, json } => pull(&target, &model, json).await,
            LocalCommand::Rm { model } => {
                match &target {
                    Target::Daemon(_) => {
                        target
                            .call(
                                reqwest::Method::DELETE,
                                &format!("/v1/local/models/{model}"),
                                None,
                            )
                            .await?;
                    }
                    Target::Direct(l) => l.delete(&model).await.map_err(anyhow::Error::msg)?,
                }
                println!("removed {model}");
                Ok(ExitCode::SUCCESS)
            }
            LocalCommand::Recommended { json } => recommended(&target, json).await,
            LocalCommand::Use { tier, model } => {
                need_daemon(&target, "choose tiers")?;
                target
                    .call(
                        reqwest::Method::POST,
                        "/v1/local/tiers",
                        Some(json!({ "tier": tier, "model": model })),
                    )
                    .await?;
                println!("{tier} now uses {model}");
                Ok(ExitCode::SUCCESS)
            }
            LocalCommand::Setup => {
                need_daemon(&target, "set up the recommended models")?;
                target
                    .call(reqwest::Method::POST, "/v1/local/setup", None)
                    .await?;
                println!("Downloading the recommended models. Watch with `tern local list`.");
                Ok(ExitCode::SUCCESS)
            }
            LocalCommand::Start => start_stop(&target, "start").await,
            LocalCommand::Stop => start_stop(&target, "stop").await,
        }
    })
}

fn need_daemon(target: &Target, what: &str) -> anyhow::Result<()> {
    if matches!(target, Target::Direct(_)) {
        bail!("tern changes config through its daemon to {what}. Run `tern start` first.");
    }
    Ok(())
}

async fn recommended(target: &Target, json: bool) -> anyhow::Result<ExitCode> {
    need_daemon(target, "read the catalog")?;
    let c = target
        .call(reqwest::Method::GET, "/v1/local/catalog", None)
        .await?;
    if json {
        println!("{c}");
        return Ok(ExitCode::SUCCESS);
    }
    for t in c["tiers"].as_array().into_iter().flatten() {
        let state = match (t["current"].as_str(), t["installed"].as_bool()) {
            (Some(cur), _) => format!("uses {cur}"),
            (None, Some(true)) => "downloaded".into(),
            _ => "not downloaded".into(),
        };
        println!(
            "{:<9} {:<18} {:>5.1} GB  {}  ({state})",
            t["tier"].as_str().unwrap_or(""),
            t["model"].as_str().unwrap_or(""),
            t["size_gb"].as_f64().unwrap_or(0.0),
            t["blurb"].as_str().unwrap_or(""),
        );
        if let Some(n) = t["note"].as_str() {
            println!("          note: {n}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn start_stop(target: &Target, what: &str) -> anyhow::Result<ExitCode> {
    if matches!(target, Target::Direct(_)) {
        bail!(
            "tern {what}s Ollama through its daemon, so it can stop it with tern. Run `tern start` first."
        );
    }
    target
        .call(reqwest::Method::POST, &format!("/v1/local/{what}"), None)
        .await?;
    println!("ok");
    Ok(ExitCode::SUCCESS)
}

async fn status(target: &Target, json: bool) -> anyhow::Result<ExitCode> {
    let s = target.status().await?;
    if json {
        println!("{s}");
        return Ok(ExitCode::SUCCESS);
    }
    let yes = |k: &str| s[k].as_bool().unwrap_or(false);
    let state = match (yes("installed"), yes("running")) {
        (_, true) => format!(
            "running{}{}",
            s["version"]
                .as_str()
                .map(|v| format!(" ({v})"))
                .unwrap_or_default(),
            if yes("managed") {
                ", started by tern"
            } else {
                ", started by you"
            }
        ),
        (true, false) => "installed, not running (`tern local start`)".into(),
        (false, false) => format!(
            "not installed (get it at {})",
            s["install_page"].as_str().unwrap_or("ollama.com/download")
        ),
    };
    println!("Ollama:   {state}");
    if let Some(c) = s["context"].as_u64() {
        println!("Context:  {c} tokens");
    }
    if let Some(f) = s["disk_free"].as_u64() {
        println!("Disk:     {} free", gb(f));
    }
    if let Some(r) = s["ram_bytes"].as_u64() {
        println!("Memory:   {}", gb(r));
    }
    for w in s["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        println!("warning:  {w}");
    }
    Ok(ExitCode::SUCCESS)
}

async fn list(target: &Target, json: bool) -> anyhow::Result<ExitCode> {
    let m = target.models().await?;
    if json {
        println!("{m}");
        return Ok(ExitCode::SUCCESS);
    }
    let models = m.as_array().cloned().unwrap_or_default();
    if models.is_empty() {
        println!("No models yet. `tern local pull qwen3:8b` downloads one.");
    }
    for x in models {
        let tools = match x["tools"].as_bool() {
            Some(true) => "tools",
            Some(false) => "no tools",
            None => "",
        };
        println!(
            "{:<32} {:>9}  {:<8} {}",
            x["name"].as_str().unwrap_or(""),
            gb(x["size"].as_u64().unwrap_or(0)),
            tools,
            if x["loaded"].as_bool().unwrap_or(false) {
                "in memory"
            } else {
                ""
            }
        );
    }
    Ok(ExitCode::SUCCESS)
}

async fn pull(target: &Target, model: &str, json: bool) -> anyhow::Result<ExitCode> {
    let job = target.pull(model).await?;
    let id = job["id"].as_str().context("no download id")?.to_owned();
    let tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let cancelled = tokio::spawn({
        let c = tokio::signal::ctrl_c();
        async move { c.await.is_ok() }
    });
    loop {
        let jobs = target.pulls().await?;
        let Some(p) = jobs
            .as_array()
            .and_then(|a| a.iter().find(|p| p["id"] == id.as_str()))
        else {
            bail!("the download disappeared");
        };
        let phase = p["phase"].as_str().unwrap_or("");
        let (done, total) = (
            p["completed"].as_u64().unwrap_or(0),
            p["total"].as_u64().unwrap_or(0),
        );
        if json {
            println!("{p}");
        } else if tty {
            let pct = (done * 100).checked_div(total).unwrap_or(0);
            eprint!("\r{phase:<12} {pct:>3}%  {} of {}   ", gb(done), gb(total));
        }
        match phase {
            "done" => {
                if tty && !json {
                    eprintln!();
                }
                println!("{model} is ready");
                return Ok(ExitCode::SUCCESS);
            }
            "failed" => {
                if tty && !json {
                    eprintln!();
                }
                bail!("{}", p["error"].as_str().unwrap_or("the download failed"));
            }
            "cancelled" => {
                bail!("cancelled; run it again to resume");
            }
            _ => {}
        }
        if cancelled.is_finished() {
            target.cancel(&id).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
