//! `tern skills`: skills and skill capture (docs/personal-agent-tooling-spec.md
//! §7, T5b). Listing, reading, publishing and discarding work on the skills
//! folder directly; offers and saving one need the daemon, which watches
//! runs and has the model.

use std::process::ExitCode;

use anyhow::{Context, bail};
use serde_json::{Value, json};
use tern_core::TernHome;
use tern_core::skills;

use crate::cli::SkillsCommand;
use crate::daemon::api_skills::{Accepted, Offer};
use crate::lifecycle::{block_on, socket_client};

pub fn run(home: &TernHome, command: Option<SkillsCommand>) -> anyhow::Result<ExitCode> {
    let root = home.skills_dir();
    match command.unwrap_or(SkillsCommand::List) {
        SkillsCommand::List => {
            let all = skills::discover_all(&root);
            if all.is_empty() {
                println!("No skills yet.");
            }
            for s in &all {
                let tag = if s.draft { "  [draft]" } else { "" };
                println!("{:<28}{tag}  {}", s.name, s.description);
            }
            if all.iter().any(|s| s.draft) {
                println!(
                    "\nDrafts are not in the prompt until published: `tern skills publish <name>`."
                );
            }
        }
        SkillsCommand::Show { name } => {
            let all = skills::discover_all(&root);
            let s = all
                .iter()
                .find(|s| s.name == name)
                .with_context(|| format!("no skill named {name}"))?;
            print!("{}", std::fs::read_to_string(s.dir.join("SKILL.md"))?);
        }
        SkillsCommand::Publish { name } => {
            skills::publish_draft(&root, &name)?;
            println!("Published {name}; the next run lists it.");
        }
        SkillsCommand::Discard { name } => {
            skills::discard_draft(&root, &name)?;
            println!("Discarded the draft {name}.");
        }
        SkillsCommand::Offers { all } => {
            let http = daemon(home, "see offers")?;
            let path = if all {
                "/v1/skill-offers?state=all"
            } else {
                "/v1/skill-offers"
            };
            let offers: Vec<Offer> = block_on(call(http.get(url(path))))??;
            if offers.is_empty() {
                println!("No skill offers. Long, clean runs are offered once they finish.");
            }
            for o in &offers {
                println!("{}", offer_line(o));
            }
        }
        SkillsCommand::Save { id, name } => {
            let http = daemon(home, "write a draft")?;
            let path = format!("/v1/skill-offers/{id}/accept");
            let a: Accepted =
                block_on(call(http.post(url(&path)).json(&json!({ "slug": name }))))??;
            println!(
                "Wrote the draft {0}. Read it with `tern skills show {0}`, then `tern skills publish {0}` (or `discard`).",
                a.skill.name
            );
        }
        SkillsCommand::Dismiss { id } => {
            let http = daemon(home, "dismiss an offer")?;
            let path = format!("/v1/skill-offers/{id}/dismiss");
            let _: Value = block_on(call(http.post(url(&path)).json(&json!({}))))??;
            println!("Dismissed offer #{id}.");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn daemon(home: &TernHome, what: &str) -> anyhow::Result<reqwest::Client> {
    socket_client(home)
        .with_context(|| format!("tern isn't running; start it with `tern start` to {what}"))
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

/// `#3   pending    Weekly digest · 11 tool calls`
pub fn offer_line(o: &Offer) -> String {
    let what = o.title.as_deref().unwrap_or("untitled conversation");
    let end = match (o.state.as_str(), &o.skill) {
        ("accepted", Some(s)) => format!(" → draft {s}"),
        _ => String::new(),
    };
    format!(
        "#{:<4}{:<10} {what} · {} tool calls{end}",
        o.id, o.state, o.tool_calls
    )
}
