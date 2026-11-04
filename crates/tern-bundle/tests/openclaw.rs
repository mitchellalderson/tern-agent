//! `tern import openclaw` against a fixture OpenClaw directory
//! (tests/fixtures/openclaw): skills and automations become a bundle,
//! profile files go to tern's, and nothing secret comes across.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use tern_bundle::openclaw::{BUNDLE_NAME, apply_profile, as_raw, scan};
use tern_bundle::{InstallOptions, Installer, source};
use tern_core::TernHome;
use tern_store::Store;

/// Every fake credential in the fixture.
const FAKES: &[&str] = &[
    "sk-ant-fake-0000000000000000",
    "123456:FAKE-telegram-bot-token-abcdef",
    "FAKE.discord.token.0000",
    "FAKE-weather-key-1234",
    "ghp_FAKE000000000000000000",
];

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &target);
        } else {
            std::fs::copy(e.path(), target).unwrap();
        }
    }
}

#[tokio::test]
async fn imports_what_maps_and_reports_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let oc = tmp.path().join("openclaw");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openclaw"),
        &oc,
    );
    // Kept out of git (.gitignore), so made here.
    std::fs::write(
        oc.join("workspace/skills/github-triage/.env"),
        "GITHUB_TOKEN=ghp_FAKE000000000000000000\n",
    )
    .unwrap();
    let home = TernHome::new(tmp.path().join("tern"));
    home.ensure_layout().unwrap();

    let imp = scan(&oc, &home).unwrap();
    assert_eq!(
        imp.workspace,
        Some(std::fs::canonicalize(oc.join("workspace")).unwrap())
    );

    // Skills: the workspace's weather wins over the managed one; the
    // managed ClawHub skill comes with a warning; the one holding a key
    // from openclaw.json doesn't come at all; .env is left behind.
    let paths: Vec<&str> = imp.files.keys().map(String::as_str).collect();
    assert_eq!(
        paths,
        vec![
            "routines.toml",
            "skills/clawhub-summarize/SKILL.md",
            "skills/github-triage/SKILL.md",
            "skills/github-triage/scripts/triage.sh",
            "skills/weather/SKILL.md",
        ]
    );
    let weather = String::from_utf8_lossy(&imp.files["skills/weather/SKILL.md"]).into_owned();
    assert!(weather.contains("forecast API"));
    let skipped = imp.skipped.join("\n");
    for want in [
        "skill leaky: not imported",
        ".env",
        "plugin voice-call",
        "AGENTS.md",
        "HEARTBEAT.md",
        "memory/*.md",
        "credentials/: never imported",
        "\"Old reminder\": a one-off whose time has passed",
        "\"Odd interval\"",
        "\"Application errors\": it watches a command's output",
        "\"Morning brief\": ran in Europe/Lisbon",
    ] {
        assert!(skipped.contains(want), "missing {want:?} in:\n{skipped}");
    }

    // Routines: cron as is, every 30m as */30, chat-delivered pushes.
    let routines = String::from_utf8_lossy(&imp.files["routines.toml"]).into_owned();
    assert!(routines.contains("cron = \"0 8 * * 1-5\""), "{routines}");
    assert!(routines.contains("cron = \"*/30 * * * *\""), "{routines}");
    assert!(routines.contains("Sweep the inbox"), "{routines}");
    assert!(
        routines.contains("initiative = \"interrupt\""),
        "{routines}"
    );

    // Secrets are names to add; setup says what to do.
    let names: Vec<&str> = imp.secrets.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, vec!["GITHUB_TOKEN", "OPENWEATHER_API_KEY"]);
    let setup = imp.setup.join("\n");
    for want in [
        "channel telegram",
        "channel discord",
        "tern mcp add linear --url https://mcp.linear.app/mcp",
        "tern mcp add github -- github-mcp-server stdio",
        "tern setup",
        "runs gh through the shell",
        "clawhub-summarize came from OpenClaw's managed skills",
    ] {
        assert!(setup.contains(want), "missing {want:?} in:\n{setup}");
    }

    // Nothing anywhere holds a credential.
    let report = imp.render();
    for fake in FAKES {
        assert!(!report.contains(fake), "the report shows {fake}");
        for (p, b) in &imp.files {
            assert!(
                !String::from_utf8_lossy(b).contains(fake),
                "{p} holds {fake}"
            );
        }
    }

    // It installs through the normal plan: skills, paused routines.
    let store = Store::open(&home.db_path()).unwrap();
    let i = Installer::new(&home);
    let raw = as_raw(&imp, &oc).unwrap().unwrap();
    let bundle = source::verify(raw, None).unwrap();
    let p = i.prepare(bundle, Some(&store)).unwrap();
    assert_eq!(p.plan.name, BUNDLE_NAME);
    assert_eq!(p.plan.signature.state, "unsigned");
    assert!(
        p.plan
            .skills
            .iter()
            .any(|s| s.slug == "github-triage" && s.runnable == vec!["scripts/triage.sh"])
    );
    assert_eq!(p.plan.routines.len(), 2);
    i.install(&p, &store, &InstallOptions::default()).unwrap();
    assert!(home.skills_dir().join("github-triage/SKILL.md").exists());
    assert!(!home.skills_dir().join("github-triage/.env").exists());
    assert!(store.schedules().unwrap().iter().all(|r| !r.enabled));
    assert!(i.list()[0].source.starts_with("openclaw:"));

    // Profile files: persona and user were tern's defaults, so replaced;
    // memory was empty. Importing again doesn't repeat them.
    let done = apply_profile(&imp).unwrap();
    assert_eq!(done.len(), 3, "{done:?}");
    let persona = std::fs::read_to_string(home.persona_path()).unwrap();
    assert!(persona.contains("You are Molty") && persona.contains("Emoji"));
    assert!(
        std::fs::read_to_string(home.user_path())
            .unwrap()
            .contains("Lisbon")
    );
    assert!(
        std::fs::read_to_string(home.memory_path())
            .unwrap()
            .contains("Dentist")
    );
    let again = apply_profile(&scan(&oc, &home).unwrap()).unwrap();
    assert!(again.iter().all(|l| l.contains("already")), "{again:?}");
}

#[test]
fn a_workspace_alone_works_and_keeps_your_persona() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("SOUL.md"), "Be brief.\n").unwrap();
    std::fs::write(ws.join("USER.md"), "Likes tea.\n").unwrap();
    let home = TernHome::new(tmp.path().join("tern"));
    home.ensure_layout().unwrap();
    std::fs::write(home.persona_path(), "My own persona.\n").unwrap();
    std::fs::write(home.user_path(), "Name: Sam\n").unwrap();

    let imp = scan(&ws, &home).unwrap();
    assert!(imp.files.is_empty());
    assert!(as_raw(&imp, &ws).unwrap().is_none());
    apply_profile(&imp).unwrap();
    assert_eq!(
        std::fs::read_to_string(home.persona_path()).unwrap(),
        "My own persona.\n"
    );
    assert!(
        std::fs::read_to_string(home.root().join("persona.openclaw.md"))
            .unwrap()
            .contains("Be brief")
    );
    let user = std::fs::read_to_string(home.user_path()).unwrap();
    assert!(
        user.starts_with("Name: Sam")
            && user.contains("Imported from OpenClaw")
            && user.contains("Likes tea")
    );
}

#[test]
fn not_openclaw_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = TernHome::new(tmp.path().join("tern"));
    assert!(scan(tmp.path(), &home).is_err());
}
