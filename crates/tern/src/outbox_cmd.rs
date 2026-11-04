//! `tern outbox`: writes waiting for approval (docs/personal-agent-tooling-spec.md
//! §5.2), through the daemon, which executes approved items.

use std::process::ExitCode;

use anyhow::{Context, bail};
use serde_json::{Value, json};
use tern_core::TernHome;
use tern_core::outbox::OutboxItem;

use crate::cli::OutboxCommand;
use crate::lifecycle::{block_on, socket_client};

pub fn run(home: &TernHome, command: Option<OutboxCommand>) -> anyhow::Result<ExitCode> {
    let http = socket_client(home)
        .context("tern isn't running; start it with `tern start` to see or decide the outbox")?;
    block_on(async move {
        match command.unwrap_or(OutboxCommand::List { all: false }) {
            OutboxCommand::List { all } => {
                let path = if all {
                    "/v1/outbox?state=all"
                } else {
                    "/v1/outbox"
                };
                let items: Vec<OutboxItem> = call(http.get(url(path))).await?;
                if items.is_empty() {
                    println!(
                        "{}",
                        if all {
                            "The outbox is empty."
                        } else {
                            "Nothing is waiting for you."
                        }
                    );
                }
                for i in &items {
                    println!("{}", line(i));
                }
            }
            OutboxCommand::Show { id } => {
                let i: OutboxItem = call(http.get(url(&format!("/v1/outbox/{id}")))).await?;
                println!("{}", line(&i));
                if let Some(t) = &i.taint {
                    println!("  ⚠ {}", tern_core::approval::taint_note(t));
                }
                println!("  input {}", serde_json::to_string_pretty(&i.input)?);
                if let Some(r) = &i.result {
                    println!(
                        "  result{} {}",
                        if r.is_error { " (error)" } else { "" },
                        r.content
                    );
                }
            }
            OutboxCommand::Approve { id, input } => {
                let body = match input {
                    Some(s) => {
                        let v: Value = serde_json::from_str(&s).context("--input must be JSON")?;
                        json!({ "input": v })
                    }
                    None => json!({}),
                };
                let path = format!("/v1/outbox/{id}/approve");
                let i: OutboxItem = call(http.post(url(&path)).json(&body)).await?;
                println!("Approved outbox #{}; it runs now: {}", i.id, i.headline());
            }
            OutboxCommand::Reject { id, note } => {
                let path = format!("/v1/outbox/{id}/reject");
                let body = json!({ "note": note });
                let i: OutboxItem = call(http.post(url(&path)).json(&body)).await?;
                println!("Rejected outbox #{}: {}", i.id, i.headline());
            }
        }
        Ok(ExitCode::SUCCESS)
    })?
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

/// `#12  pending  write · gmail_send to bob@…  (expires in 71h)`
fn line(i: &OutboxItem) -> String {
    let when = match i.state.as_str() {
        "pending" => {
            let left = (i.expires_at - tern_store::now_ms()).max(0) / 3_600_000;
            format!("  (expires in {left}h)")
        }
        _ => String::new(),
    };
    let flags = [
        i.taint.as_ref().map(|_| " [tainted run]"),
        i.local_only.then_some(" [this computer only]"),
        i.edited.then_some(" [edited]"),
    ]
    .into_iter()
    .flatten()
    .collect::<String>();
    format!("#{:<4} {:<9} {}{flags}{when}", i.id, i.state, i.headline())
}
