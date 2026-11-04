//! `tern bundle export`: your own things, never a secret, and what comes
//! out installs again.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use tern_bundle::export::{Request, export};
use tern_bundle::manifest::Header;
use tern_bundle::source::{self, Signed};
use tern_bundle::{BundleError, InstallOptions, Installer, SecretKey};
use tern_core::{Secrets, TernHome};
use tern_store::Store;
use tern_tools::schedule::{ScheduleLimits, TriggerArgs, create_routine};

const TOKEN: &str = "secret_ntn_4f8a9c2d7e1b";

const NOTION: &str = r#"return {
  name = "notion",
  description = "Notion pages",
  capabilities = { http = { "api.notion.com" }, auth = { "NOTION_TOKEN" } },
  tools = { search = { description = "Search", effect = "read", run = function(ctx, a) return {} end } },
}"#;

fn setup() -> (tempfile::TempDir, TernHome, Store) {
    let tmp = tempfile::tempdir().unwrap();
    let home = TernHome::new(tmp.path().join("tern"));
    home.ensure_layout().unwrap();
    Secrets::load(&home.secrets_path())
        .unwrap()
        .set("NOTION_TOKEN", TOKEN)
        .unwrap();
    std::fs::create_dir_all(home.connectors_dir()).unwrap();
    std::fs::write(home.connectors_dir().join("notion.luau"), NOTION).unwrap();
    let skill = home.skills_dir().join("brief");
    std::fs::create_dir_all(skill.join("scripts")).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: brief\ndescription: Morning brief\n---\nCheck Notion.\n",
    )
    .unwrap();
    std::fs::write(skill.join("scripts/run.sh"), "#!/bin/sh\necho hi\n").unwrap();
    std::fs::write(skill.join(".env"), format!("NOTION_TOKEN={TOKEN}\n")).unwrap();
    std::fs::write(skill.join("client_secret.json"), "{\"x\":1}").unwrap();
    let store = Store::open(&home.db_path()).unwrap();
    (tmp, home, store)
}

fn header() -> Header {
    Header {
        name: "my-brief".into(),
        version: "0.1.0".into(),
        description: "My morning brief".into(),
        ..Header::default()
    }
}

#[test]
fn export_contains_no_secrets() {
    let (_tmp, home, store) = setup();
    let cron = create_routine(
        &store,
        &TriggerArgs::cron("0 8 * * 1-5"),
        "Summarize Notion changes",
        Some("telegram:12345".into()),
        &ScheduleLimits {
            runs_per_day: Some(Some(3)),
            ..ScheduleLimits::default()
        },
        tern_store::DEFAULT_AGENT,
    )
    .unwrap();
    let hook = create_routine(
        &store,
        &TriggerArgs::webhook(),
        "A webhook arrived",
        None,
        &ScheduleLimits::default(),
        tern_store::DEFAULT_AGENT,
    )
    .unwrap();

    let key = SecretKey::generate().unwrap();
    let out = export(
        &home,
        Some(&store),
        &Request {
            header: header(),
            all: true,
            ..Request::default()
        },
        Some(&key),
    )
    .unwrap();

    let paths: Vec<&str> = out.files.keys().map(String::as_str).collect();
    assert_eq!(
        paths,
        vec![
            "bundle.toml",
            "bundle.toml.minisig",
            "connectors/notion.luau",
            "routines.toml",
            "skills/brief/SKILL.md",
            "skills/brief/scripts/run.sh",
        ]
    );
    for (p, bytes) in &out.files {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains(TOKEN), "{p} holds the secret");
        assert!(
            !text.contains("telegram:12345"),
            "{p} holds the delivery target"
        );
        assert!(!text.contains(&cron.session_id), "{p} holds a session id");
    }
    assert!(out.notes.iter().any(|n| n.contains(".env")));
    assert!(out.notes.iter().any(|n| n.contains("client_secret.json")));
    assert!(
        out.notes
            .iter()
            .any(|n| n.contains(&hook.id) && n.contains("webhook")),
        "{:?}",
        out.notes
    );
    let routines = String::from_utf8_lossy(&out.files["routines.toml"]).into_owned();
    assert!(routines.contains("cron = \"0 8 * * 1-5\""), "{routines}");
    assert!(routines.contains("runs_per_day = 3"), "{routines}");
    assert!(!routines.contains("agent"), "{routines}");

    // It reads back as a signed bundle.
    let packed = source::pack(&out.files).unwrap();
    let b = source::verify(
        source::unpack(tern_bundle::Origin::Generated("t".into()), &packed).unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(b.signed, Signed::By(key.public));
}

#[test]
fn a_secret_value_in_a_file_refuses_the_export() {
    let (_tmp, home, store) = setup();
    std::fs::write(
        home.skills_dir().join("brief/notes.md"),
        format!("my token is {TOKEN}\n"),
    )
    .unwrap();
    let r = export(
        &home,
        Some(&store),
        &Request {
            header: header(),
            skills: vec!["brief".into()],
            ..Request::default()
        },
        None,
    );
    match r {
        Err(BundleError::Refused(why)) => {
            assert!(
                why.contains("NOTION_TOKEN") && !why.contains(TOKEN),
                "{why}"
            );
        }
        Err(e) => panic!("wrong error: {e}"),
        Ok(_) => panic!("exported a secret"),
    }
}

#[tokio::test]
async fn an_export_installs_elsewhere() {
    let (tmp, home, store) = setup();
    let out = export(
        &home,
        Some(&store),
        &Request {
            header: header(),
            connectors: vec!["notion".into()],
            skills: vec!["brief".into()],
            ..Request::default()
        },
        None,
    )
    .unwrap();
    let file = tmp.path().join("my-brief.tar.zst");
    let sha = out.write_archive(&file).unwrap();

    let other = TernHome::new(tmp.path().join("other"));
    other.ensure_layout().unwrap();
    let ostore = Store::open(&other.db_path()).unwrap();
    let i = Installer::new(&other);
    let b = i
        .read(&format!("{}#sha256={sha}", file.display()), None)
        .await
        .unwrap();
    let p = i.prepare(b, Some(&ostore)).unwrap();
    assert_eq!(p.plan.secrets[0].name, "NOTION_TOKEN");
    assert!(!p.plan.secrets[0].set, "the other side has no value");
    i.install(&p, &ostore, &InstallOptions::default()).unwrap();
    assert!(other.drafts_dir().join("notion.luau").exists());
    assert!(other.skills_dir().join("brief/scripts/run.sh").exists());
}
