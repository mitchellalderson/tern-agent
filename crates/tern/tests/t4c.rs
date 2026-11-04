//! T4c through the real binary: `tern connectors from-openapi` writes a
//! draft, `tern connectors test --record` saves sanitized fixtures from the
//! live API, and `tern connectors test` replays them offline (what CI runs).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn tern_bin() -> PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

fn tern(home: &Path, args: &[&str]) -> Output {
    Command::new(tern_bin())
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("TERN_HOME", home.join(".tern"))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn from_openapi_then_record_then_replay_offline() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/pets/1"))
        .and(header("x-api-key", "cli-secret-999"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "name": "Rex", "session_token": "tok-abc"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let tern_home = home.join(".tern");
    std::fs::create_dir_all(&tern_home).unwrap();
    let secrets = tern_home.join("secrets.toml");
    std::fs::write(&secrets, "PETS_API_KEY = \"cli-secret-999\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let spec = tmp.path().join("pets.yaml");
    std::fs::write(
        &spec,
        format!(
            r#"openapi: 3.0.0
info: {{ title: Pets, version: "1" }}
servers: [{{ url: "{}/api" }}]
security: [{{ key: [] }}]
components:
  securitySchemes:
    key: {{ type: apiKey, in: header, name: X-API-Key }}
paths:
  /pets/{{petId}}:
    get:
      operationId: getPet
      parameters: [{{ name: petId, in: path, required: true, schema: {{ type: string }} }}]
    delete:
      operationId: deletePet
      parameters: [{{ name: petId, in: path, required: true, schema: {{ type: string }} }}]
"#,
            server.uri()
        ),
    )
    .unwrap();
    let spec = spec.to_str().unwrap();

    let list = tern(&home, &["connectors", "from-openapi", spec, "--list"]);
    assert!(list.status.success(), "{}", text(&list));
    assert!(
        text(&list).contains("get_pet [read] GET /pets/{petId}"),
        "{}",
        text(&list)
    );
    assert!(
        text(&list).contains("delete_pet [destructive] DELETE"),
        "{}",
        text(&list)
    );

    // Without --ops: the reads.
    let made = tern(&home, &["connectors", "from-openapi", spec]);
    assert!(made.status.success(), "{}", text(&made));
    let out = text(&made);
    assert!(out.contains("draft saved and valid"), "{out}");
    assert!(out.contains("lua__pets__get_pet [read]"), "{out}");
    assert!(!out.contains("delete_pet ["), "{out}");
    assert!(out.contains("PETS_API_KEY"), "{out}");
    let draft = tern_home.join("connectors/drafts/pets.luau");
    assert!(draft.exists());
    assert!(
        !tern_home.join("connectors/pets.luau").exists(),
        "never activated"
    );

    // No fixtures yet: valid, and says how to record.
    let first = tern(&home, &["connectors", "test", "pets"]);
    assert!(first.status.success(), "{}", text(&first));
    assert!(text(&first).contains("no fixtures yet"), "{}", text(&first));

    let rec = tern(
        &home,
        &[
            "connectors",
            "test",
            "pets",
            "--record",
            "--tool",
            "get_pet",
            "--args",
            r#"{"petId":"1"}"#,
        ],
    );
    assert!(rec.status.success(), "{}", text(&rec));
    assert!(text(&rec).contains("recorded get_pet"), "{}", text(&rec));
    let fixtures = tern_home.join("connectors/drafts/pets.fixtures.json");
    let saved = std::fs::read_to_string(&fixtures).unwrap();
    assert!(
        !saved.contains("cli-secret-999") && !saved.contains("tok-abc"),
        "{saved}"
    );
    assert!(saved.contains("Rex"), "{saved}");

    // Offline: the server is gone and the secret removed.
    drop(server);
    std::fs::write(&secrets, "").unwrap();
    let replay = tern(&home, &["connectors", "test", "pets"]);
    assert!(replay.status.success(), "{}", text(&replay));
    assert!(text(&replay).contains("✓ get_pet"), "{}", text(&replay));

    // A changed result fails, as CI would.
    let mut f: serde_json::Value = serde_json::from_str(&saved).unwrap();
    f["cases"][0]["result"] = "something else".into();
    std::fs::write(&fixtures, f.to_string()).unwrap();
    let changed = tern(&home, &["connectors", "test", "pets"]);
    assert!(!changed.status.success(), "{}", text(&changed));
    assert!(
        text(&changed).contains("differs from the recording"),
        "{}",
        text(&changed)
    );
}
