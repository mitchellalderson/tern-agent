//! The config editor's API (`/v1/config/settings`): reading settings,
//! changing and resetting one while keeping the file's comments, refusing
//! bad values and unknown or secret keys, applying live settings without a
//! restart, and owner scope only.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::PathBuf;
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

struct Env {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    tern_home: PathBuf,
    script: PathBuf,
    port: u16,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let tern_home = home.join(".tern");
        std::fs::create_dir_all(&tern_home).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        std::fs::write(
            tern_home.join("config.toml"),
            format!(
                "# my settings\n[api]\nlisten = [\"unix\", \"tcp://127.0.0.1:{port}\"]\n\n\
                 [sandbox]\n# I want this on\nnetwork = true # for now\n"
            ),
        )
        .unwrap();
        let script = tmp.path().join("script.json");
        std::fs::write(&script, json!({ "turns": [] }).to_string()).unwrap();
        Self {
            script,
            home,
            tern_home,
            port,
            _tmp: tmp,
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_tern"));
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TERN_HOME", &self.tern_home)
            .env("TERN_PROVIDER", format!("mock:{}", self.script.display()))
            .stdin(Stdio::null());
        c
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().unwrap();
        assert!(
            out.status.success(),
            "tern {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn daemon(&self) -> Killed {
        let child = self
            .cmd(&["daemon"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(10);
        while std::net::TcpStream::connect(("127.0.0.1", self.port)).is_err()
            || !self.tern_home.join("tern.sock").exists()
        {
            assert!(Instant::now() < end, "daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Killed(child)
    }

    fn local(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .unix_socket(self.tern_home.join("tern.sock"))
            .build()
            .unwrap()
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.tern_home.join("config.toml")).unwrap()
    }
}

async fn send(req: reqwest::RequestBuilder) -> (u16, Value) {
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

fn setting<'a>(view: &'a Value, key: &str) -> &'a Value {
    view["sections"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["settings"].as_array().unwrap())
        .find(|s| s["key"] == key)
        .unwrap_or_else(|| panic!("no {key} in {view}"))
}

const URL: &str = "http://tern/v1/config/settings";

#[tokio::test(flavor = "multi_thread")]
async fn read_change_and_reset_settings() {
    let env = Env::new();
    let _daemon = env.daemon();
    let http = env.local();

    let (status, view) = send(http.get(URL)).await;
    assert_eq!(status, 200, "{view}");
    assert!(view["path"].as_str().unwrap().ends_with("config.toml"));
    let titles: Vec<&str> = view["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["title"].as_str().unwrap())
        .collect();
    assert!(titles.contains(&"Shell & sandbox"), "{titles:?}");
    assert!(!titles.iter().any(|t| t.contains("rovider")), "{titles:?}");
    let net = setting(&view, "sandbox.network");
    assert_eq!(net["kind"], "bool");
    assert_eq!(
        (net["value"].clone(), net["set"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(net["risky"], true);
    assert!(net["security"].as_str().unwrap().contains("internet"));
    assert_eq!(net["applies"], "restart");
    assert!(!net["description"].as_str().unwrap().is_empty());
    assert_eq!(setting(&view, "max_iterations")["set"], false);

    // A restart setting: saved with the comments kept, pending a restart.
    let (status, s) = send(
        http.put(format!("{URL}/sandbox.network"))
            .json(&json!({ "value": false })),
    )
    .await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(
        (s["value"].clone(), s["risky"].clone()),
        (json!(false), json!(false))
    );
    assert_eq!(s["restart_pending"], true);
    let text = env.config();
    for kept in [
        "# my settings",
        "# I want this on",
        "# for now",
        "network = false",
    ] {
        assert!(text.contains(kept), "{kept}: {text}");
    }

    // A live one applies to the next run.
    let (status, s) = send(
        http.put(format!("{URL}/default_model"))
            .json(&json!({ "value": "mock:other" })),
    )
    .await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(
        (s["applies"].clone(), s["restart_pending"].clone()),
        (json!("live"), json!(false))
    );
    let (_, info) = send(http.get("http://tern/v1/info")).await;
    assert_eq!(info["default_model"], "mock:other");

    // Bad values, unknown keys and secrets are refused; nothing is written.
    let before = env.config();
    for (key, value, want) in [
        ("sandbox.network", json!("yes"), 400),
        ("subagents.max_depth", json!(7), 400),
        ("sandbox.confine", json!("sometimes"), 400),
        ("schedules.quiet_hours", json!("late"), 400),
        ("sandbox.netwrk", json!(true), 404),
        ("providers.anthropic.base_url", json!("http://x"), 404),
        ("gateway.telegram.bot_token_secret", json!("X"), 400),
    ] {
        let (status, e) = send(
            http.put(format!("{URL}/{key}"))
                .json(&json!({ "value": value })),
        )
        .await;
        assert_eq!(status, want, "{key}: {e}");
        assert!(e["error"].is_string(), "{key}: {e}");
    }
    assert_eq!(env.config(), before);

    // Reset: back to the default, out of the file.
    let (status, s) = send(http.delete(format!("{URL}/sandbox.network"))).await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(
        (s["value"].clone(), s["set"].clone()),
        (json!(true), json!(false))
    );
    assert_eq!(s["restart_pending"], false);
    let text = env.config();
    assert!(!text.contains("network ="), "{text}");
    assert!(text.contains("# my settings"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_owner_sees_or_changes_config() {
    let env = Env::new();
    let _daemon = env.daemon();
    let http = reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let url = |p: &str| format!("https://127.0.0.1:{}{p}", env.port);
    for scope in ["chat", "read", "owner"] {
        let link = env.ok(&["devices", "pair", "--scope", scope, "--name", scope]);
        let token = env.ok(&["devices", "join", &link]);
        let auth = format!("Bearer {token}");
        let (get, e) = send(
            http.get(url("/v1/config/settings"))
                .header("authorization", &auth),
        )
        .await;
        let (put, _) = send(
            http.put(url("/v1/config/settings/max_iterations"))
                .header("authorization", &auth)
                .json(&json!({ "value": 12 })),
        )
        .await;
        if scope == "owner" {
            assert_eq!((get, put), (200, 200), "{e}");
        } else {
            assert_eq!((get, put), (403, 403), "{scope}: {e}");
        }
    }
    assert!(env.config().contains("max_iterations = 12"));
}
