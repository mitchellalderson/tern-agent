//! Bundles over the daemon API (tooling spec §6.5): preview shows the plan,
//! install takes the preview's hash and nothing else, and remove takes
//! back what was installed. The desktop's install flow is these calls.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::collections::BTreeMap;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const CONNECTOR: &str = r#"return {
  name = "weather",
  description = "Forecasts",
  capabilities = { http = { "api.open-meteo.com/v1/*" }, rate = "30/min" },
  tools = { forecast = { description = "The forecast", effect = "read", run = function(ctx, a) return {} end } },
}"#;

const ROUTINES: &str = r#"
[[routine]]
name = "morning-weather"
prompt = "Tell me the weather."
trigger = { cron = "0 7 * * *" }
initiative = "interrupt"
"#;

async fn send(req: reqwest::RequestBuilder) -> (u16, Value) {
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread")]
async fn preview_install_and_remove_over_the_api() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let tern_home = home.join(".tern");
    std::fs::create_dir_all(&tern_home).unwrap();
    // No run uses the model here.
    let script = tmp.path().join("script.json");
    std::fs::write(&script, r#"{"turns": []}"#).unwrap();

    // A bundle directory.
    let dir = tmp.path().join("weather-bundle");
    let files = tern_bundle::source::assemble(
        tern_bundle::manifest::Header {
            name: "weather".into(),
            version: "1.0.0".into(),
            description: "Morning weather".into(),
            ..Default::default()
        },
        BTreeMap::from([
            (
                "connectors/weather.luau".to_owned(),
                CONNECTOR.as_bytes().to_vec(),
            ),
            ("routines.toml".to_owned(), ROUTINES.as_bytes().to_vec()),
        ]),
        None,
    )
    .unwrap();
    for (p, b) in &files {
        let t = dir.join(p);
        std::fs::create_dir_all(t.parent().unwrap()).unwrap();
        std::fs::write(t, b).unwrap();
    }

    let child = Command::new(env!("CARGO_BIN_EXE_tern"))
        .arg("daemon")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("TERN_HOME", &tern_home)
        .env("TERN_PROVIDER", format!("mock:{}", script.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _daemon = Killed(child);
    let end = Instant::now() + Duration::from_secs(10);
    while !tern_home.join("tern.sock").exists() {
        assert!(Instant::now() < end, "daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    let http = reqwest::Client::builder()
        .unix_socket(tern_home.join("tern.sock"))
        .build()
        .unwrap();
    let wait = Instant::now() + Duration::from_secs(10);
    loop {
        let (s, health) = send(http.get("http://tern/v1/health")).await;
        if s == 200 {
            assert!(
                health["features"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("bundles"))
            );
            break;
        }
        assert!(Instant::now() < wait, "no health");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (s, list) = send(http.get("http://tern/v1/bundles")).await;
    assert_eq!((s, list), (200, json!({"bundles": []})));

    let source = dir.to_string_lossy().into_owned();
    let (s, p) = send(
        http.post("http://tern/v1/bundles/preview")
            .json(&json!({"source": source})),
    )
    .await;
    assert_eq!(s, 200, "{p}");
    assert_eq!(p["can_install"], true);
    assert_eq!(p["grows"], true);
    assert_eq!(p["plan"]["signature"]["state"], "unsigned");
    assert_eq!(
        p["plan"]["connectors"][0]["hosts"],
        json!(["api.open-meteo.com/v1/*"])
    );
    assert_eq!(p["plan"]["connectors"][0]["tools"][0]["effect"], "read");
    assert_eq!(p["plan"]["routines"][0]["trigger"], "cron 0 7 * * *");
    let sha = p["plan"]["sha256"].as_str().unwrap().to_owned();
    assert!(
        !tern_home.join("connectors/drafts/weather.luau").exists(),
        "a preview installs nothing"
    );

    // Install wants the preview's hash, and only that bundle.
    let (s, e) = send(
        http.post("http://tern/v1/bundles/install")
            .json(&json!({"source": source, "sha256": ""})),
    )
    .await;
    assert_eq!(s, 400, "{e}");
    std::fs::write(dir.join("README.md"), "changed since the preview").unwrap();
    let (s, e) = send(
        http.post("http://tern/v1/bundles/install")
            .json(&json!({"source": source, "sha256": sha})),
    )
    .await;
    assert_eq!(s, 400, "a file added after the preview: {e}");
    std::fs::remove_file(dir.join("README.md")).unwrap();
    let (s, out) = send(
        http.post("http://tern/v1/bundles/install")
            .json(&json!({"source": source, "sha256": sha})),
    )
    .await;
    assert_eq!(s, 200, "{out}");
    assert_eq!(out["name"], "weather");
    assert!(tern_home.join("connectors/drafts/weather.luau").exists());

    let (_, scheds) = send(http.get("http://tern/v1/schedules")).await;
    let r = scheds
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["cron"] == "0 7 * * *")
        .unwrap()
        .clone();
    assert_eq!(r["enabled"], false, "{r}");

    let (_, list) = send(http.get("http://tern/v1/bundles")).await;
    assert_eq!(list["bundles"][0]["name"], "weather");
    assert_eq!(list["bundles"][0]["sha256"], sha);
    assert_eq!(list["bundles"][0]["signature"], "unsigned");

    let (s, out) = send(http.delete("http://tern/v1/bundles/weather")).await;
    assert_eq!(s, 200, "{out}");
    assert!(!tern_home.join("connectors/drafts/weather.luau").exists());
    let (s, _) = send(http.delete("http://tern/v1/bundles/weather")).await;
    assert_eq!(s, 404);
}
