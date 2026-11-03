//! `tern providers` and `tern models`: provider setup for scripts. They
//! talk to the running daemon when there is one, else to an embedded
//! runtime, so changes apply to a running daemon at once.

use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use tern_core::TernHome;
use tern_core::config::ProviderKind;
use tern_core::models::ProviderDraft;
use tern_tui::Backend;

use crate::cli::{ProviderKindArg, ProvidersCommand};

fn with_backend<T>(
    home: &TernHome,
    f: impl AsyncFnOnce(Arc<dyn Backend>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        // Approvals never come up: these commands run no tools.
        let (backend, _approvals) = crate::tui_backend::connect(home, None).await?;
        f(backend).await
    })
}

fn kind_label(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::OpenaiCompat => "openai-compat",
    }
}

pub fn run(home: &TernHome, command: Option<ProvidersCommand>) -> anyhow::Result<ExitCode> {
    match command.unwrap_or(ProvidersCommand::List) {
        ProvidersCommand::List => list(home),
        ProvidersCommand::Add {
            kind,
            url,
            name,
            default_model,
            env_key,
            no_key,
        } => {
            let kind = match kind {
                ProviderKindArg::Anthropic => ProviderKind::Anthropic,
                ProviderKindArg::OpenaiCompat => ProviderKind::OpenaiCompat,
            };
            if kind == ProviderKind::OpenaiCompat && url.is_none() {
                bail!("openai-compat needs --url, e.g. --url http://localhost:11434/v1");
            }
            if kind == ProviderKind::Anthropic && no_key {
                bail!("Anthropic needs an API key");
            }
            let key = if env_key || no_key {
                None
            } else {
                Some(read_key(kind)?)
            };
            let draft = ProviderDraft {
                kind: Some(kind),
                name,
                base_url: url,
                key: key.filter(|k| !k.trim().is_empty()),
                use_env_key: env_key,
            };
            add(home, draft, default_model)
        }
        ProvidersCommand::Rm { name, keep_key } => with_backend(home, async |b| {
            b.remove_provider(&name, !keep_key)
                .map_err(anyhow::Error::msg)?;
            println!("removed {name}");
            Ok(ExitCode::SUCCESS)
        }),
    }
}

fn list(home: &TernHome) -> anyhow::Result<ExitCode> {
    with_backend(home, async |b| {
        let providers = b.providers().map_err(anyhow::Error::msg)?;
        if providers.is_empty() {
            println!("No providers yet. Run `tern setup` to connect one.");
            return Ok(ExitCode::SUCCESS);
        }
        for p in providers {
            let key = match (p.key_source.as_str(), &p.key_hint, &p.secret) {
                ("env", Some(h), Some(s)) => format!("key {h} from ${s}"),
                ("file", Some(h), Some(s)) => format!("key {h} ({s})"),
                ("missing", _, Some(s)) => format!("key missing ({s})"),
                _ => "no key".into(),
            };
            let default = if p.is_default { "  [default]" } else { "" };
            println!(
                "{:16} {:14} {}  {key}{default}",
                p.name,
                kind_label(p.kind),
                p.base_url.as_deref().unwrap_or("-")
            );
            if let Some(m) = p.message {
                println!("{:16} {m}", "");
            }
        }
        if let Some(m) = b.default_model() {
            println!("\ndefault model: {m}");
        }
        Ok(ExitCode::SUCCESS)
    })
}

fn add(
    home: &TernHome,
    draft: ProviderDraft,
    default_model: Option<String>,
) -> anyhow::Result<ExitCode> {
    with_backend(home, async |b| {
        let v = b
            .verify_provider(draft.clone(), None)
            .await
            .map_err(anyhow::Error::msg)?;
        for w in &v.warnings {
            eprintln!("warning: {w}");
        }
        if !v.ok {
            eprintln!(
                "error: {}",
                v.message.as_deref().unwrap_or("verification failed")
            );
            return Ok(ExitCode::FAILURE);
        }
        if let Some(m) = &v.message {
            eprintln!("note: {m}");
        }
        if let Some(url) = &v.suggested_base_url {
            eprintln!("note: using {url} (added /v1)");
        }
        let status = b
            .save_provider(draft, None, default_model)
            .await
            .map_err(anyhow::Error::msg)?;
        let key = status
            .key_hint
            .as_deref()
            .map(|h| format!(", key {h}"))
            .unwrap_or_default();
        println!("added {} ({} models{key})", status.name, v.models.len());
        if status.is_default
            && let Some(m) = b.default_model()
        {
            println!("default model: {m}");
        }
        Ok(ExitCode::SUCCESS)
    })
}

/// The key from stdin when it's piped, else typed or pasted without echo.
fn read_key(kind: ProviderKind) -> anyhow::Result<String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut key = String::new();
        stdin.lock().read_to_string(&mut key)?;
        return Ok(key);
    }
    let prompt = match kind {
        ProviderKind::Anthropic => "Paste your Anthropic API key (hidden): ",
        ProviderKind::OpenaiCompat => "API key, or Enter for none (hidden): ",
    };
    read_hidden(prompt)
}

fn read_hidden(prompt: &str) -> anyhow::Result<String> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
    let mut err = std::io::stderr();
    write!(err, "{prompt}")?;
    err.flush()?;
    crossterm::terminal::enable_raw_mode().context("reading the key")?;
    let mut key = String::new();
    let result = loop {
        match read() {
            Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow::anyhow!("cancelled"));
                }
                KeyCode::Backspace => {
                    key.pop();
                }
                KeyCode::Char(c) => key.push(c),
                _ => {}
            },
            Ok(Event::Paste(s)) => key.push_str(&s),
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    writeln!(err)?;
    result.map(|()| key)
}

pub fn models(
    home: &TernHome,
    provider: Option<String>,
    refresh: bool,
    json: bool,
) -> anyhow::Result<ExitCode> {
    with_backend(home, async |b| {
        let lists = b
            .models(provider, refresh)
            .await
            .map_err(anyhow::Error::msg)?;
        if json {
            println!("{}", serde_json::to_string_pretty(&lists)?);
            return Ok(ExitCode::SUCCESS);
        }
        if lists.is_empty() {
            println!("No providers yet. Run `tern setup` to connect one.");
        }
        for l in lists {
            println!("{}", l.provider);
            if let Some(m) = &l.message {
                println!("  ({m})");
            }
            for m in &l.models {
                let mut note = String::new();
                if let Some(n) = m.name.as_ref().filter(|n| **n != m.id) {
                    note.push_str(n);
                }
                if let Some(w) = m.context_window {
                    if !note.is_empty() {
                        note.push_str(", ");
                    }
                    note.push_str(&format!("{}k context", w / 1000));
                }
                if note.is_empty() {
                    println!("  {}:{}", l.provider, m.id);
                } else {
                    println!("  {}:{}  {note}", l.provider, m.id);
                }
            }
        }
        Ok(ExitCode::SUCCESS)
    })
}
