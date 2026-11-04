//! `tern report`: one redacted file with what's needed to debug a problem.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::Context;
use tern_core::{Config, Secrets, TernHome};
use tern_store::Store;

const LOG_TAIL_LINES: usize = 200;
const MAX_CRASHES: usize = 2;

pub fn run(
    home: &TernHome,
    output: Option<&Path>,
    run_id: Option<&str>,
) -> anyhow::Result<ExitCode> {
    let mut out = String::new();
    let now = chrono::Local::now();
    let _ = writeln!(out, "# tern bug report\n");
    let _ = writeln!(
        out,
        "Generated {}. Secret values tern knows about, and strings that look like API keys or tokens, are replaced with `[REDACTED]`. **Review this before sharing it.**\n",
        now.format("%Y-%m-%d %H:%M:%S %Z")
    );

    let _ = writeln!(out, "## Environment\n");
    let _ = writeln!(out, "- tern {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "- {} {}", std::env::consts::OS, std::env::consts::ARCH);
    let _ = writeln!(out, "- data directory: {}", home.root().display());
    let _ = writeln!(
        out,
        "- TERN_LOG: {}",
        std::env::var("TERN_LOG").unwrap_or_else(|_| "(unset: info)".into())
    );
    let _ = writeln!(
        out,
        "- TERN_PROVIDER: {}\n",
        std::env::var("TERN_PROVIDER")
            .map(|_| "set")
            .unwrap_or("unset")
    );

    section(
        &mut out,
        "tern doctor --offline",
        &tern(&["doctor", "--offline"]),
    );
    let config =
        std::fs::read_to_string(home.config_path()).unwrap_or_else(|_| "(no config.toml)".into());
    section_lang(&mut out, "config.toml", &config, "toml");
    section(
        &mut out,
        "tern connectors list",
        &tern(&["connectors", "list"]),
    );

    match problem_run(home, run_id) {
        Some((id, why)) => section(
            &mut out,
            &format!("Run {id} ({why})"),
            &tern(&["runs", "show", &id]),
        ),
        None => {
            let _ = writeln!(out, "## Runs\n\nNo runs recorded.\n");
        }
    }

    let crashes = crate::crash::recent(&home.logs_dir(), MAX_CRASHES);
    if crashes.is_empty() {
        let _ = writeln!(out, "## Crashes\n\nNo crash files.\n");
    }
    for c in crashes {
        let name = c
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        section(
            &mut out,
            &format!("Crash: {name}"),
            &std::fs::read_to_string(&c).unwrap_or_default(),
        );
    }

    match latest_log(&home.logs_dir()) {
        Some(log) => {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            let tail = lines[lines.len().saturating_sub(LOG_TAIL_LINES)..].join("\n");
            let name = log
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            section(
                &mut out,
                &format!(
                    "Log: last {} lines of {name}",
                    LOG_TAIL_LINES.min(lines.len())
                ),
                &tail,
            );
        }
        None => {
            let _ = writeln!(out, "## Log\n\nNo log files yet.\n");
        }
    }

    let report = redact(home, &out);
    match output {
        Some(p) if p == Path::new("-") => print!("{report}"),
        _ => {
            let path = match output {
                Some(p) => p.to_path_buf(),
                None => {
                    let dir = if home.logs_dir().is_dir() {
                        home.logs_dir()
                    } else {
                        std::env::temp_dir()
                    };
                    dir.join(format!("report-{}.md", now.format("%Y%m%d-%H%M%S")))
                }
            };
            write_private(&path, &report).with_context(|| format!("writing {}", path.display()))?;
            println!("{}", path.display());
            eprintln!(
                "Wrote a bug report to the path above. Review it, then share it (paste it into the conversation, or attach it)."
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Run this binary with `args`, capturing everything it prints.
fn tern(args: &[&str]) -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("tern"));
    match Command::new(exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                s.push_str("\n[stderr]\n");
                s.push_str(&err);
            }
            s
        }
        Err(e) => format!("(could not run: {e})"),
    }
}

fn section(out: &mut String, title: &str, body: &str) {
    section_lang(out, title, body, "");
}

fn section_lang(out: &mut String, title: &str, body: &str, lang: &str) {
    let _ = writeln!(out, "## {title}\n\n```{lang}\n{}\n```\n", body.trim_end());
}

/// `--run`, else the newest run that didn't complete, else the newest run.
fn problem_run(home: &TernHome, run_id: Option<&str>) -> Option<(String, String)> {
    if let Some(id) = run_id {
        return Some((id.to_owned(), "requested".into()));
    }
    let store = Store::open(&home.db_path())
        .ok()
        .filter(|_| home.db_path().exists())?;
    let runs = store.recent_runs(50).ok()?;
    if let Some(r) = runs.iter().find(|r| r.status != "completed") {
        return Some((r.id.clone(), format!("most recent {} run", r.status)));
    }
    runs.first()
        .map(|r| (r.id.clone(), "most recent run".into()))
}

fn latest_log(dir: &Path) -> Option<PathBuf> {
    let mut logs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_some_and(|n| {
                n.to_string_lossy().starts_with("tern.") && n.to_string_lossy().ends_with(".log")
            })
        })
        .collect();
    logs.sort();
    logs.pop()
}

/// Every secret tern knows (secrets.toml, plus env values the config or a
/// connector names), then patterns that look like credentials.
fn redact(home: &TernHome, text: &str) -> String {
    let secrets = Secrets::load(&home.secrets_path()).unwrap_or_default();
    let (config, _) = Config::load(&home.config_path()).unwrap_or_default();
    let mut names = config.secret_names();
    names.extend(connector_secret_names(home));
    for n in &names {
        // Reading a secret makes `redact` cover it.
        let _ = secrets.get(n);
    }
    redact_patterns(&secrets.redact(text))
}

fn connector_secret_names(home: &TernHome) -> Vec<String> {
    let Ok(store) = Store::open_in_memory() else {
        return Vec::new();
    };
    let Ok(host) = tern_lua::Host::new(store, Secrets::default()) else {
        return Vec::new();
    };
    [home.connectors_dir(), home.drafts_dir()]
        .iter()
        .flat_map(|d| {
            std::fs::read_dir(d)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
        })
        .filter(|p| p.extension().is_some_and(|e| e == tern_lua::EXTENSION))
        .filter_map(|p| tern_lua::load_file(&host, &p).ok())
        .flat_map(|vm| vm.spec.capabilities.all_secrets())
        .collect()
}

/// Catch credentials tern doesn't know about: well-known token prefixes, and
/// `key=…`-style values in URLs, headers, and config.
pub fn redact_patterns(text: &str) -> String {
    static TOKENS: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\b(sk-ant-[A-Za-z0-9_\-]{8,}|sk-[A-Za-z0-9_\-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|xox[abprs]-[A-Za-z0-9\-]{10,}|glpat-[A-Za-z0-9_\-]{16,}|AKIA[0-9A-Z]{16}|\d{6,}:[A-Za-z0-9_\-]{30,})").ok()
    });
    static PAIRS: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(?i)\b((?:api[_-]?)?key|token|secret|password|passwd|authorization|bearer)(\s*[=:]\s*|=)("?)([^\s&"',;]{8,})"#).ok()
    });
    static SCHEMES: std::sync::LazyLock<Option<regex::Regex>> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\b(bearer|basic)\s+([A-Za-z0-9._~+/=\-]{8,})").ok()
    });
    let mut s = text.to_owned();
    if let Some(re) = SCHEMES.as_ref() {
        s = re.replace_all(&s, "$1 [REDACTED]").into_owned();
    }
    if let Some(re) = TOKENS.as_ref() {
        s = re.replace_all(&s, "[REDACTED]").into_owned();
    }
    if let Some(re) = PAIRS.as_ref() {
        s = re.replace_all(&s, "$1$2$3[REDACTED]").into_owned();
    }
    s
}

fn write_private(path: &Path, body: &str) -> std::io::Result<()> {
    std::fs::write(path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_catch_common_credentials() {
        let text = "url https://blog.example.com/ghost/api/content/posts/?key=22444f78447824223cefc48062&limit=3\n\
                    Authorization: Bearer abcdefghijklmnop\n\
                    sk-ant-api03-ABCDEFGHIJKLMNOP and ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123\n\
                    bot token 123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsawQ\n\
                    password = \"hunter2hunter2\"\n\
                    keep: limit=3 and model=anthropic:claude";
        let r = redact_patterns(text);
        for leaked in [
            "22444f78447824223cefc48062",
            "abcdefghijklmnop",
            "ABCDEFGHIJKLMNOP",
            "ghp_ABC",
            "AAHdqTcvCH1",
            "hunter2hunter2",
        ] {
            assert!(!r.contains(leaked), "{leaked} leaked:\n{r}");
        }
        assert!(
            r.contains("limit=3") && r.contains("model=anthropic:claude"),
            "{r}"
        );
        assert!(r.contains("key=[REDACTED]"), "{r}");
    }
}
