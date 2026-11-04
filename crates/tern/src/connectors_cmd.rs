//! `tern connectors [list | test <name> [--record] | from-openapi <url|file>]`.

use std::process::ExitCode;

use tern_core::{Config, Secrets, TernHome};
use tern_lua::fixtures::{self, Fixtures, Outcome};
use tern_lua::{EXTENSION, Host, load_file, load_fingerprinted, openapi};
use tern_store::Store;

/// A host for loading connectors to inspect them: real secrets (so missing
/// ones can be reported) and the configured default rate, but a scratch
/// store, so nothing is written.
pub(crate) fn host(home: &TernHome) -> anyhow::Result<Host> {
    let secrets = Secrets::load(&home.secrets_path())?;
    let mut host = Host::new(Store::open_in_memory()?, secrets)?;
    if let Ok((config, _)) = Config::load(&home.config_path()) {
        host.default_rate = config.connectors.rate;
    }
    Ok(host)
}

/// The warning for a connector that reads secret values itself.
pub(crate) fn legacy_note(spec: &tern_lua::ConnectorSpec) -> Option<String> {
    let c = &spec.capabilities;
    c.legacy_secrets().then(|| {
        format!(
            "legacy: the script reads {} itself with ctx.secret, so it could send the value to any host it may reach. Move it to capabilities.auth and tern adds it to requests without the script seeing it",
            c.secrets.join(", ")
        )
    })
}

pub(crate) fn luau_files_in(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    luau_files(dir)
}

fn luau_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == EXTENSION))
        .collect();
    files.sort();
    files
}

/// One line per tool: name, effect and flags.
fn tool_lines(spec: &tern_lua::ConnectorSpec) -> Vec<String> {
    spec.tools
        .iter()
        .map(|t| {
            let flags = [
                (t.untrusted, "untrusted"),
                (t.read_via_post, "read via POST"),
                (t.idempotent, "idempotent"),
                (t.requires_approval, "requires approval"),
                (t.forbidden.is_some(), "refused: moves money or trades"),
            ]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, f)| *f)
            .collect::<Vec<_>>()
            .join(", ");
            format!(
                "- {} [{}]: {}{}",
                tern_lua::tool_name(&spec.name, &t.name),
                t.effect(),
                t.description,
                if flags.is_empty() {
                    String::new()
                } else {
                    format!(" ({flags})")
                }
            )
        })
        .collect()
}

pub fn list(home: &TernHome) -> anyhow::Result<ExitCode> {
    let host = host(home)?;
    let secrets = Secrets::load(&home.secrets_path())?;
    // Read-only look at grandfathering (T0); never creates a database.
    let store = home
        .db_path()
        .exists()
        .then(|| Store::open(&home.db_path()).ok())
        .flatten();
    for (label, dir) in [
        ("active", home.connectors_dir()),
        ("drafts", home.drafts_dir()),
    ] {
        let files = luau_files(&dir);
        println!("{label} ({}):", dir.display());
        if files.is_empty() {
            println!("  (none)");
        }
        for path in files {
            let file = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            match load_fingerprinted(&host, &path) {
                Err(e) => println!("  ✗ {file}: {e}"),
                Ok(l) => {
                    let s = &l.vm.spec;
                    println!("  ✓ {} — {}", s.name, s.description);
                    println!("      {}", l.vm.capability_summary());
                    if let Some(note) = legacy_note(s) {
                        println!("      ! {note}");
                    }
                    for line in tool_lines(s) {
                        println!("      {line}");
                    }
                    if label == "active"
                        && store.as_ref().is_some_and(|st| {
                            tern_lua::is_grandfathered(st, &s.name, &l.fingerprint.hash)
                        })
                    {
                        println!(
                            "      grandfathered: tools without `effect` don't ask, as before T0, until it is activated again or edited"
                        );
                    }
                    let missing: Vec<String> = s
                        .capabilities
                        .all_secrets()
                        .into_iter()
                        .filter(|k| secrets.get(k).is_none())
                        .collect();
                    if !missing.is_empty() {
                        println!("      ! secrets not set: {}", missing.join(", "));
                    }
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Load and validate a connector (active first, then draft) and print its
/// tools, then replay its fixtures offline, or with `record` call the live
/// API and save them.
pub fn test(
    home: &TernHome,
    name: &str,
    record: bool,
    tool: Option<&str>,
    args: Option<&str>,
) -> anyhow::Result<ExitCode> {
    let host = host(home)?;
    let candidates = [
        home.connectors_dir().join(format!("{name}.{EXTENSION}")),
        home.drafts_dir().join(format!("{name}.{EXTENSION}")),
    ];
    let Some(path) = candidates.iter().find(|p| p.exists()) else {
        eprintln!(
            "no connector named {name:?} in {} or its drafts/",
            home.connectors_dir().display()
        );
        return Ok(ExitCode::FAILURE);
    };
    let vm = match load_file(&host, path) {
        Err(e) => {
            eprintln!("✗ {}: {e}", path.display());
            return Ok(ExitCode::FAILURE);
        }
        Ok(vm) => vm,
    };
    println!("✓ {} is valid ({})", vm.spec.name, path.display());
    println!("  {}", vm.capability_summary());
    if let Some(note) = legacy_note(&vm.spec) {
        println!("  ! {note}");
    }
    for line in tool_lines(&vm.spec) {
        println!("  {line}");
    }
    let fixtures_path = fixtures::path_for(path);
    let existing = match Fixtures::load(&fixtures_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("✗ {e}");
            return Ok(ExitCode::FAILURE);
        }
    };
    if record {
        let args = match args
            .map(serde_json::from_str::<serde_json::Value>)
            .transpose()
        {
            Ok(a) => a.unwrap_or_else(|| serde_json::json!({})),
            Err(e) => {
                eprintln!("--args isn't JSON: {e}");
                return Ok(ExitCode::FAILURE);
            }
        };
        return record_fixtures(home, path, name, existing, tool, args);
    }
    let Some(fixtures) = existing else {
        println!(
            "  no fixtures yet; record some with `tern connectors test {name} --record`, or by testing the draft in a conversation"
        );
        return Ok(ExitCode::SUCCESS);
    };
    replay_fixtures(&vm, &fixtures, &fixtures_path)
}

/// Replay every recorded case offline; any failure or changed result fails.
fn replay_fixtures(
    vm: &tern_lua::Vm,
    fixtures: &Fixtures,
    path: &std::path::Path,
) -> anyhow::Result<ExitCode> {
    println!(
        "  replaying {} recorded call{} from {} (offline)",
        fixtures.cases.len(),
        if fixtures.cases.len() == 1 { "" } else { "s" },
        path.display()
    );
    let mut failed = 0;
    for case in &fixtures.cases {
        let label = format!("{} {}", case.tool, case.args);
        match crate::lifecycle::block_on(fixtures::check_case(vm, case))? {
            Outcome::Pass => println!("  ✓ {label}"),
            Outcome::Changed { got } => {
                failed += 1;
                println!("  ✗ {label}: the result differs from the recording");
                println!("      recorded: {}", one_line(&case.result, 300));
                println!("      now:      {}", one_line(&got, 300));
            }
            Outcome::Failed(e) => {
                failed += 1;
                println!("  ✗ {label}: {e}");
            }
        }
    }
    if failed > 0 {
        println!("  {failed} of {} failed", fixtures.cases.len());
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    format!("{}…", flat.chars().take(max).collect::<String>())
}

/// Call the live API for each case and save the fixtures. Write tools are
/// recorded only when named with --tool: re-recording never repeats a
/// write on its own.
fn record_fixtures(
    home: &TernHome,
    path: &std::path::Path,
    name: &str,
    existing: Option<Fixtures>,
    tool: Option<&str>,
    args: serde_json::Value,
) -> anyhow::Result<ExitCode> {
    // The real store and accounts, so OAuth connectors run signed in.
    let secrets = Secrets::load(&home.secrets_path())?;
    let (config, _) = Config::load(&home.config_path()).unwrap_or_default();
    let store = if home.db_path().exists() {
        Store::open(&home.db_path())?
    } else {
        Store::open_in_memory()?
    };
    let mut host = Host::new(store.clone(), secrets.clone())?;
    host.default_rate = config.connectors.rate;
    host.connections = Some(crate::runtime::open_connections(&config, &store, &secrets));
    let vm = load_file(&host, path)?;
    let reads: Vec<&tern_lua::ToolDef> = vm
        .spec
        .tools
        .iter()
        .filter(|t| t.effect() == tern_core::tool::Effect::Read)
        .collect();
    let cases: Vec<(String, serde_json::Value)> = match (tool, &existing) {
        (Some(t), _) => vec![(t.to_owned(), args)],
        (None, Some(f)) if !f.cases.is_empty() => f
            .cases
            .iter()
            .filter(|c| reads.iter().any(|t| t.name == c.tool))
            .map(|c| (c.tool.clone(), c.args.clone()))
            .collect(),
        _ => reads
            .iter()
            .filter(|t| t.params["required"].as_array().is_none_or(|r| r.is_empty()))
            .map(|t| (t.name.clone(), serde_json::json!({})))
            .collect(),
    };
    if cases.is_empty() {
        println!(
            "  nothing to record: no read tool runs without arguments. Name one: --tool <tool> --args '{{...}}'"
        );
        return Ok(ExitCode::FAILURE);
    }
    let conns = host.connections.clone();
    let mut fixtures = existing.unwrap_or_else(|| Fixtures::new(name));
    let mut failed = 0;
    for (tool, args) in cases {
        let conn = match &conns {
            Some(c) => match c.resolve(
                tern_auth::Kind::Connector,
                name,
                None,
                tern_core::tool::Effect::Read,
            ) {
                Ok(r) => r,
                Err(e) => {
                    println!("  ✗ {tool}: {e}");
                    failed += 1;
                    continue;
                }
            },
            None => None,
        };
        match crate::lifecycle::block_on(fixtures::record_case(&vm, &tool, &args, conn))? {
            Ok((_, case)) => {
                println!(
                    "  ● recorded {tool} {args} ({} HTTP exchange{})",
                    case.exchanges.len(),
                    if case.exchanges.len() == 1 { "" } else { "s" }
                );
                fixtures.upsert(case);
            }
            Err(e) => {
                println!("  ✗ {tool} {args}: {e}");
                failed += 1;
            }
        }
    }
    let file = fixtures::path_for(path);
    if let Err(e) = fixtures.save(&file) {
        eprintln!("✗ {e}");
        return Ok(ExitCode::FAILURE);
    }
    println!(
        "  saved {} (credentials and secret values removed; replay with `tern connectors test {name}`)",
        file.display()
    );
    Ok(if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// `tern connectors from-openapi`'s options.
pub struct FromOpenapi {
    pub source: String,
    pub name: Option<String>,
    pub ops: Vec<String>,
    pub list: bool,
    pub tag: Option<String>,
    pub search: Option<String>,
    pub offset: usize,
    pub base_url: Option<String>,
}

pub fn from_openapi(home: &TernHome, o: &FromOpenapi) -> anyhow::Result<ExitCode> {
    let (config, _) = Config::load(&home.config_path()).unwrap_or_default();
    let is_url = o.source.starts_with("https://") || o.source.starts_with("http://");
    let text = if is_url {
        let f = crate::lifecycle::block_on(tern_core::net::get_public(
            &o.source,
            openapi::MAX_DOCUMENT,
            false,
            std::time::Duration::from_secs(60),
        ))?
        .map_err(|e| anyhow::anyhow!("fetching {}: {e}", o.source))?;
        if !f.status.is_success() {
            anyhow::bail!("fetching {}: HTTP {}", o.source, f.status);
        }
        if f.truncated {
            anyhow::bail!(
                "{} is larger than {} bytes",
                o.source,
                openapi::MAX_DOCUMENT
            );
        }
        String::from_utf8_lossy(&f.body).into_owned()
    } else {
        std::fs::read_to_string(&o.source)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", o.source))?
    };
    let doc = openapi::parse(
        &text,
        is_url.then_some(o.source.as_str()),
        o.base_url.as_deref(),
    )
    .map_err(anyhow::Error::msg)?;
    let filter = openapi::Filter {
        tag: o.tag.clone(),
        search: o.search.clone(),
    };
    let max = config.connectors.openapi_max_tools.max(1);
    if o.list {
        print!("{}", openapi::listing(&doc, &filter, o.offset, 50));
        return Ok(ExitCode::SUCCESS);
    }
    if o.ops.is_empty() && doc.operations.len() > max {
        print!("{}", openapi::listing(&doc, &filter, o.offset, 50));
        eprintln!(
            "This API is too large to generate whole; nothing was written. Choose up to {max} operations with --ops (names, \"GET /path\" or tag:<tag>)."
        );
        return Ok(ExitCode::FAILURE);
    }
    let chosen = openapi::select(
        &doc,
        (!o.ops.is_empty()).then_some(o.ops.as_slice()),
        config.connectors.openapi_tools.max(1),
        max,
    )
    .map_err(anyhow::Error::msg)?;
    let name = o.name.clone().unwrap_or_else(|| doc.suggested_name());
    if !tern_lua::valid_ident(&name) {
        anyhow::bail!("name {name:?} must match [a-z][a-z0-9_]* (max 32 chars)");
    }
    let dirs = tern_lua::tools::Dirs {
        active: home.connectors_dir(),
        drafts: home.drafts_dir(),
    };
    let host = host(home)?;
    let source = if is_url {
        o.source.clone()
    } else {
        String::new()
    };
    let (vm, generated) =
        tern_lua::tools::draft_from_openapi(&host, &dirs, &doc, &name, &chosen, &source)
            .map_err(anyhow::Error::msg)?;
    println!(
        "{}",
        tern_lua::tools::openapi_report(&vm, &doc, &chosen, &generated)
    );
    println!(
        "\nFrom the command line: `tern connectors test {name} --record` calls the live API and saves fixtures; to activate, ask tern to activate the {name} connector (you approve its capabilities)."
    );
    Ok(ExitCode::SUCCESS)
}
