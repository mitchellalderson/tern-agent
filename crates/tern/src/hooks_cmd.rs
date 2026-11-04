//! `tern hooks`: webhook routines (docs/personal-agent-tooling-spec.md §3.1,
//! T1c), through the daemon, which owns the webhook listener.

use std::process::ExitCode;

use anyhow::{Context, bail};
use serde_json::{Value, json};
use tern_core::TernHome;

use crate::cli::HooksCommand;
use crate::lifecycle::{block_on, socket_client};

pub fn run(home: &TernHome, command: Option<HooksCommand>) -> anyhow::Result<ExitCode> {
    let http = socket_client(home)
        .context("tern isn't running; start it with `tern start` to manage webhooks")?;
    block_on(async move {
        match command.unwrap_or(HooksCommand::List) {
            HooksCommand::List => {
                let hooks: Vec<Value> = call(http.get(url("/v1/webhooks"))).await?;
                if hooks.is_empty() {
                    println!(
                        "No webhook routines. Make one with `tern hooks create \"<prompt>\"`."
                    );
                }
                for h in &hooks {
                    let state = if h["active"] == true {
                        "active"
                    } else {
                        "revoked"
                    };
                    println!(
                        "{}  {state:<8} {:<7} {}",
                        h["routine_id"].as_str().unwrap_or_default(),
                        h["auth"].as_str().unwrap_or_default(),
                        where_(h)
                    );
                }
            }
            HooksCommand::Create {
                prompt,
                auth,
                agent,
                channel,
                gate,
                gate_connector,
                initiative,
            } => {
                let body = json!({
                    "prompt": prompt,
                    "auth": auth,
                    "agent": agent,
                    "channel": channel,
                    "gate": gate,
                    "gate_connector": gate_connector,
                    "initiative": initiative,
                });
                let created: Value = call(http.post(url("/v1/webhooks")).json(&body)).await?;
                let h = &created["webhook"];
                println!(
                    "Created webhook routine {}.",
                    h["routine_id"].as_str().unwrap_or_default()
                );
                show_secret(h);
            }
            HooksCommand::Rotate { id, auth } => {
                let path = format!("/v1/webhooks/{id}/rotate");
                let h: Value = call(http.post(url(&path)).json(&json!({ "auth": auth }))).await?;
                println!("New secret for {id}; the old one no longer works.");
                show_secret(&h);
            }
            HooksCommand::Revoke { id } => {
                let _: Value = call(http.delete(url(&format!("/v1/webhooks/{id}")))).await?;
                println!("Revoked: {id}'s URL answers 404 until `tern hooks rotate {id}`.");
            }
            HooksCommand::Fire { id, body, file } => {
                let body = match (body, file) {
                    (Some(b), _) => b,
                    (None, Some(f)) => std::fs::read_to_string(&f)
                        .with_context(|| format!("reading {}", f.display()))?,
                    (None, None) => "{}".into(),
                };
                let path = format!("/v1/webhooks/{id}/fire");
                let out: Value = call(
                    http.post(url(&path))
                        .header("content-type", "application/json")
                        .body(body),
                )
                .await?;
                println!(
                    "{}",
                    match out["status"].as_str() {
                        Some("queued") => "Delivered; a run starts for it.",
                        Some("ignored") => "Delivered; the gate ignored it (no run).",
                        Some("duplicate") => "Already delivered; nothing new started.",
                        _ => "Delivered.",
                    }
                );
            }
        }
        Ok(ExitCode::SUCCESS)
    })?
}

/// The URL, or why there isn't one yet.
fn where_(h: &Value) -> String {
    match h["url"].as_str() {
        Some(u) => u.to_owned(),
        None => format!(
            "{} (no [webhooks] listener: only `tern hooks fire` reaches it)",
            h["path"].as_str().unwrap_or_default()
        ),
    }
}

fn show_secret(h: &Value) {
    let auth = h["auth"].as_str().unwrap_or_default();
    println!("  URL     {}", where_(h));
    println!(
        "  auth    {auth} ({})",
        if auth == "bearer" {
            "send `Authorization: Bearer <secret>`"
        } else {
            "sign the body: X-Tern-Signature: t=<unix secs>,v1=<hex HMAC-SHA256 of \"<t>.<body>\">, or GitHub's X-Hub-Signature-256"
        }
    );
    println!("  secret  {}", h["secret"].as_str().unwrap_or_default());
    println!("This is the only time the secret is shown.");
}

fn url(path: &str) -> String {
    format!("http://tern{path}")
}

async fn call<T: serde::de::DeserializeOwned>(req: reqwest::RequestBuilder) -> anyhow::Result<T> {
    let resp = req.send().await.context("asking tern")?;
    let status = resp.status();
    if !status.is_success() {
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let msg = body["error"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| status.to_string());
        bail!("{msg}");
    }
    Ok(resp.json().await?)
}
