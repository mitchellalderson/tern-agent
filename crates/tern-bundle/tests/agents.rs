//! Agent templates as bundles (docs/agent-profiles-design.md AP3):
//! `agents/<slug>/agent.toml` and `persona.md` become real agents at
//! install, with the same plan, key pinning and removal as everything else.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;

use tern_bundle::manifest::Header;
use tern_bundle::source::{assemble, pack};
use tern_bundle::{BundleError, InstallOptions, Installer};
use tern_core::TernHome;
use tern_store::{AgentChange, Store};

const AGENT: &str = r#"
name = "Researcher"
description = "Looks things up"
memory = "private"
autonomy = "readonly"
avatar = { glyph = "★", color = "blue" }

[tools]
builtins = "read_only"
connectors = []
mcp = []
"#;

const PERSONA: &str = "You are Researcher. Find things out and cite sources.";

const ROUTINES: &str = r#"
[[routine]]
name = "weekly"
agent = "researcher"
prompt = "Summarise what changed this week."
trigger = { cron = "0 9 * * 1" }

[[routine]]
name = "loose"
prompt = "Do something."
trigger = { cron = "0 10 * * 1" }
"#;

struct Env {
    _tmp: tempfile::TempDir,
    home: TernHome,
    store: Store,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let home = TernHome::new(tmp.path().join("tern"));
    home.ensure_layout().unwrap();
    let store = Store::open(&home.db_path()).unwrap();
    Env {
        _tmp: tmp,
        home,
        store,
    }
}

fn header(version: &str) -> Header {
    Header {
        name: "researcher-kit".into(),
        version: version.into(),
        description: "An agent".into(),
        author: "Alice".into(),
        key: None,
    }
}

fn files(agent: &str) -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        (
            "agents/researcher/agent.toml".to_owned(),
            agent.as_bytes().to_vec(),
        ),
        (
            "agents/researcher/persona.md".to_owned(),
            PERSONA.as_bytes().to_vec(),
        ),
        ("routines.toml".to_owned(), ROUTINES.as_bytes().to_vec()),
    ])
}

fn write(dir: &Path, name: &str, files: BTreeMap<String, Vec<u8>>) -> String {
    let path = dir.join(name);
    std::fs::write(&path, pack(&files).unwrap()).unwrap();
    path.to_string_lossy().into_owned()
}

async fn prepare(e: &Env, i: &Installer, src: &str) -> tern_bundle::Prepared {
    let b = i.read(src, None).await.unwrap();
    i.prepare(b, Some(&e.store)).unwrap()
}

#[tokio::test]
async fn an_agent_template_installs_as_an_agent_and_removes_by_archiving() {
    let e = env();
    let i = Installer::new(&e.home);
    let src = write(
        e._tmp.path(),
        "a.tar.zst",
        assemble(header("1.0.0"), files(AGENT), None).unwrap(),
    );
    let p = prepare(&e, &i, &src).await;

    // The plan says what the agent is and what it may reach.
    let a = &p.plan.agents[0];
    assert_eq!(
        (a.slug.as_str(), a.name.as_str()),
        ("researcher", "Researcher")
    );
    assert_eq!(a.autonomy.as_deref(), Some("readonly"));
    assert!(
        a.tools.contains(&"builtins: read_only".to_owned()),
        "{:?}",
        a.tools
    );
    assert!(a.tools.contains(&"connectors: none".to_owned()));
    assert!(!a.existing && !a.kept);
    let text = p.plan.render();
    assert!(text.contains("@researcher (Researcher)"), "{text}");
    assert!(text.contains("autonomy ceiling: readonly"), "{text}");
    assert!(p.plan.conflicts().is_empty());

    // Nothing exists until it's installed.
    assert!(e.store.agent("researcher").unwrap().is_none());
    let out = i.install(&p, &e.store, &InstallOptions::default()).unwrap();
    assert!(
        out.lines
            .iter()
            .any(|l| l.starts_with("agent @researcher: created")),
        "{out:?}"
    );
    let agent = e.store.agent("researcher").unwrap().unwrap();
    assert_eq!(agent.id, "tern:researcher");
    assert_eq!(agent.name, "Researcher");
    assert_eq!(agent.memory, "private");
    assert_eq!(agent.autonomy.as_deref(), Some("readonly"));
    assert!(agent.avatar.unwrap().contains("blue"));
    let scope = tern_core::agents::ToolScope::parse(&agent.tools.unwrap()).unwrap();
    assert_eq!(scope.builtins, tern_core::agents::Pick::ReadOnly);
    assert_eq!(
        std::fs::read_to_string(tern_core::agents::persona_path(&e.home, "tern:researcher"))
            .unwrap()
            .trim(),
        PERSONA
    );
    // Its DM exists.
    assert!(e.store.agent_channel("tern:researcher").unwrap().is_some());

    // Its routine is its own, paused; the loose one goes to its only agent too.
    let schedules = e.store.schedules().unwrap();
    assert_eq!(schedules.len(), 2);
    for s in &schedules {
        assert_eq!(s.agent(), "tern:researcher");
        assert!(!s.enabled);
    }
    let lock = i.installed("researcher-kit").unwrap().unwrap();
    assert_eq!(lock.agents.len(), 1);
    assert_eq!(lock.agents[0].id, "tern:researcher");

    // An update leaves the agent as the user has it, and still tracks it.
    e.store
        .update_agent(
            "tern:researcher",
            &AgentChange {
                name: Some("My researcher".into()),
                ..AgentChange::default()
            },
        )
        .unwrap();
    let src2 = write(
        e._tmp.path(),
        "b.tar.zst",
        assemble(header("1.1.0"), files(AGENT), None).unwrap(),
    );
    let p2 = prepare(&e, &i, &src2).await;
    assert!(p2.plan.agents[0].kept);
    assert!(p2.plan.conflicts().is_empty());
    let out = i
        .install(&p2, &e.store, &InstallOptions::default())
        .unwrap();
    assert!(
        out.lines
            .iter()
            .any(|l| l.contains("@researcher: already installed")),
        "{out:?}"
    );
    assert_eq!(
        e.store.agent("researcher").unwrap().unwrap().name,
        "My researcher"
    );

    // Removing archives it (history and persona stay) and drops its routines.
    let out = i.remove("researcher-kit", &e.store).unwrap();
    assert!(
        out.lines
            .iter()
            .any(|l| l.contains("@researcher: archived")),
        "{out:?}"
    );
    assert!(
        e.store
            .agent("researcher")
            .unwrap()
            .unwrap()
            .archived_at
            .is_some()
    );
    assert!(e.store.schedules().unwrap().is_empty());
    assert!(tern_core::agents::persona_path(&e.home, "tern:researcher").exists());
}

#[tokio::test]
async fn a_growing_agent_template_must_be_approved_again_and_an_existing_agent_conflicts() {
    let e = env();
    let i = Installer::new(&e.home);
    let src = write(
        e._tmp.path(),
        "a.tar.zst",
        assemble(header("1.0.0"), files(AGENT), None).unwrap(),
    );
    // You already have @researcher: refused, and the plan says so.
    e.store
        .create_agent(&tern_store::AgentRow {
            id: "tern:researcher".into(),
            slug: "researcher".into(),
            name: "Mine".into(),
            memory: "private".into(),
            ..tern_store::AgentRow::default()
        })
        .unwrap();
    let p = prepare(&e, &i, &src).await;
    assert!(p.plan.agents[0].existing);
    assert!(p.plan.conflicts().iter().any(|c| c.contains("@researcher")));
    assert!(matches!(
        i.install(&p, &e.store, &InstallOptions::default()),
        Err(BundleError::Refused(_))
    ));
    e.store
        .update_agent(
            "tern:researcher",
            &AgentChange {
                slug: Some("mine".into()),
                ..AgentChange::default()
            },
        )
        .unwrap();
    let p = prepare(&e, &i, &src).await;
    i.install(&p, &e.store, &InstallOptions::default())
        .unwrap_err();
}

#[tokio::test]
async fn a_wider_update_shows_up_as_added_capabilities() {
    let e = env();
    let i = Installer::new(&e.home);
    let src = write(
        e._tmp.path(),
        "a.tar.zst",
        assemble(header("1.0.0"), files(AGENT), None).unwrap(),
    );
    let p = prepare(&e, &i, &src).await;
    i.install(&p, &e.store, &InstallOptions::default()).unwrap();

    let wide = AGENT.replace("connectors = []", "connectors = \"all\"");
    let src2 = write(
        e._tmp.path(),
        "b.tar.zst",
        assemble(header("1.1.0"), files(&wide), None).unwrap(),
    );
    let p2 = prepare(&e, &i, &src2).await;
    let ch = p2.plan.changes.as_ref().unwrap();
    assert!(ch.grows());
    assert!(
        ch.added
            .iter()
            .any(|l| l == "agent @researcher: connectors: all"),
        "{ch:?}"
    );
    assert!(
        p2.plan
            .warnings
            .iter()
            .any(|w| w.contains("every connector you have")),
        "{:?}",
        p2.plan.warnings
    );
}

#[tokio::test]
async fn bad_agent_templates_are_refused() {
    let e = env();
    let i = Installer::new(&e.home);
    // An agent without its persona is refused when the bundle is read.
    let mut f = files(AGENT);
    f.remove("agents/researcher/persona.md");
    if let Ok(assembled) = assemble(header("1.0.0"), f, None) {
        let src = write(e._tmp.path(), "x.tar.zst", assembled);
        assert!(i.read(&src, None).await.is_err());
    }
    // Unknown keys (a model, a Telegram token, the tainted-host limit) are
    // refused: nothing machine-specific or security-widening rides along.
    for extra in [
        "model = \"x\"",
        "egress = \"allow\"",
        "telegram = \"t\"",
        "delegates = []",
    ] {
        let src = write(
            e._tmp.path(),
            "c.tar.zst",
            assemble(header("1.0.0"), files(&format!("{AGENT}\n{extra}\n")), None).unwrap(),
        );
        let b = i.read(&src, None).await.unwrap();
        assert!(i.prepare(b, Some(&e.store)).is_err(), "{extra}");
    }
    // A bad autonomy level, an empty name.
    for bad in [
        AGENT.replace("readonly", "reckless"),
        AGENT.replace("Researcher", ""),
    ] {
        let src = write(
            e._tmp.path(),
            "d.tar.zst",
            assemble(header("1.0.0"), files(&bad), None).unwrap(),
        );
        let b = i.read(&src, None).await.unwrap();
        assert!(i.prepare(b, Some(&e.store)).is_err());
    }
    // A routine naming an agent the bundle doesn't bring.
    let mut f = files(AGENT);
    f.insert(
        "routines.toml".into(),
        ROUTINES.replace("researcher", "ghost").into_bytes(),
    );
    let src = write(
        e._tmp.path(),
        "e.tar.zst",
        assemble(header("1.0.0"), f, None).unwrap(),
    );
    let b = i.read(&src, None).await.unwrap();
    assert!(i.prepare(b, Some(&e.store)).is_err());
}

#[tokio::test]
async fn an_agent_exports_and_installs_elsewhere_without_its_private_settings() {
    let e = env();
    // The agent to share: a model and a Telegram-ish setting that must not travel.
    let (a, _) = e
        .store
        .create_agent(&tern_store::AgentRow {
            id: "tern:scout".into(),
            slug: "scout".into(),
            name: "Scout".into(),
            memory: "shared".into(),
            model: Some("mock:secret-model".into()),
            tools: Some(r#"{"builtins":"read_only","connectors":[],"mcp":[]}"#.into()),
            autonomy: Some("supervised".into()),
            avatar: Some(r#"{"glyph":"●","color":"green"}"#.into()),
            ..tern_store::AgentRow::default()
        })
        .unwrap();
    let persona_path = tern_core::agents::persona_path(&e.home, &a.id);
    std::fs::create_dir_all(persona_path.parent().unwrap()).unwrap();
    std::fs::write(&persona_path, "You are Scout.\n").unwrap();
    let req = tern_bundle::export::Request {
        header: header("0.1.0"),
        agents: vec!["scout".into()],
        ..Default::default()
    };
    let ex = tern_bundle::export::export(&e.home, Some(&e.store), &req, None).unwrap();
    let toml = String::from_utf8(ex.files["agents/scout/agent.toml"].clone()).unwrap();
    assert!(toml.contains("Scout") && toml.contains("shared"), "{toml}");
    assert!(!toml.contains("secret-model"), "{toml}");
    assert!(ex.files.contains_key("agents/scout/persona.md"));

    // The default agent is never exported.
    let bad = tern_bundle::export::Request {
        header: header("0.1.0"),
        agents: vec!["tern".into()],
        ..Default::default()
    };
    assert!(tern_bundle::export::export(&e.home, Some(&e.store), &bad, None).is_err());

    // Installs into a fresh home.
    let other = env();
    let src = e._tmp.path().join("scout.tar.zst");
    ex.write_archive(&src).unwrap();
    let i = Installer::new(&other.home);
    let p = prepare(&other, &i, src.to_str().unwrap()).await;
    assert!(p.plan.warnings.iter().any(|w| w.contains("shared memory")));
    i.install(&p, &other.store, &InstallOptions::default())
        .unwrap();
    let got = other.store.agent("scout").unwrap().unwrap();
    assert_eq!(got.memory, "shared");
    assert_eq!(got.autonomy.as_deref(), Some("supervised"));
    assert_eq!(got.model, None);
}
