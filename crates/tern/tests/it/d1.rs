//! D1 end-to-end: a TCP + TLS listener, device pairing with a pinned
//! self-signed certificate, scopes, a run with the mock provider, uploads,
//! and revocation closing the device's event stream.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures::StreamExt;
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
            format!("[api]\nlisten = [\"unix\", \"tcp://127.0.0.1:{port}\"]\ntls = \"auto\"\n"),
        )
        .unwrap();
        Self {
            script: tmp.path().join("script.json"),
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

    fn url(&self, path: &str) -> String {
        format!("https://127.0.0.1:{}{path}", self.port)
    }
}

/// The pin itself is checked by `tern devices join`; plain test requests
/// skip verification.
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .build()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_over_tls_chat_upload_and_revoke() {
    let env = Env::new();
    std::fs::write(
        &env.script,
        json!({ "turns": [{ "text": "Hello from the remote core." }] }).to_string(),
    )
    .unwrap();
    let _daemon = env.daemon();
    let http = http();

    // Liveness needs no token; everything else does.
    let r = http.get(env.url("/healthz")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let r = http.get(env.url("/v1/sessions")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(r.headers()["www-authenticate"], "Bearer");

    // Owner makes a code; the link carries the URL and the pin.
    let link = env.ok(&["devices", "pair", "--scope", "chat", "--name", "test phone"]);
    assert!(
        link.starts_with("tern://pair?u=https%3A%2F%2F127.0.0.1%3A"),
        "{link}"
    );
    assert!(link.contains("&fp=sha256%3A"), "{link}");

    // A wrong pin fails the handshake without spending the code.
    let fp_start = link.find("&fp=").unwrap() + 4;
    let fp_end = link[fp_start..]
        .find('&')
        .map_or(link.len(), |i| fp_start + i);
    let wrong = format!(
        "{}sha256%3A{}{}",
        &link[..fp_start],
        "00".repeat(32),
        &link[fp_end..]
    );
    let out = env.cmd(&["devices", "join", &wrong]).output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("reaching https://127.0.0.1"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let token = env.ok(&["devices", "join", &link]);
    assert!(token.starts_with("tern_dev_"), "{token}");
    let auth = format!("Bearer {token}");

    // Single use.
    let out = env.cmd(&["devices", "join", &link]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid, used, or expired"));

    // Health says who we are.
    let health: Value = http
        .get(env.url("/v1/health"))
        .header("authorization", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["api"], 1);
    assert_eq!(health["deployment"], "server");
    assert_eq!(health["auth"], json!({ "kind": "device", "scope": "chat" }));
    assert!(
        health["features"]
            .as_array()
            .unwrap()
            .contains(&json!("uploads"))
    );

    // Chat scope can't manage devices.
    let r = http
        .get(env.url("/v1/devices"))
        .header("authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);

    // Open the event stream, then send a message and wait for the answer.
    let events = http
        .get(env.url("/v1/events"))
        .header("authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), 200);
    let mut events = events.bytes_stream();

    let started: Value = http
        .post(env.url("/v1/runs"))
        .header("authorization", &auth)
        .json(&json!({ "text": "hi from my phone" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let run_id = started["run_id"].as_str().unwrap();
    let outcome: Value = http
        .get(env.url(&format!("/v1/runs/{run_id}/wait")))
        .header("authorization", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(outcome["status"], "completed", "{outcome}");
    assert_eq!(outcome["final_text"], "Hello from the remote core.");

    // The stream saw the run.
    let mut seen = String::new();
    let end = Instant::now() + Duration::from_secs(5);
    while !seen.contains(run_id) {
        assert!(Instant::now() < end, "no events for the run: {seen}");
        if let Ok(Some(Ok(chunk))) =
            tokio::time::timeout(Duration::from_millis(500), events.next()).await
        {
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    // Upload a file into the workspace.
    let boundary = "ternboundary42";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"../notes.txt\"\r\nContent-Type: text/plain\r\n\r\nremember the milk\r\n--{boundary}--\r\n"
    );
    let up: Value = http
        .post(env.url("/v1/uploads"))
        .header("authorization", &auth)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let path = up["path"].as_str().unwrap();
    assert!(
        path.starts_with("uploads/") && path.ends_with("/notes.txt"),
        "{up}"
    );
    assert_eq!(
        std::fs::read_to_string(env.home.join("tern-workspace").join(path)).unwrap(),
        "remember the milk"
    );

    // Over 1 MB is refused for ordinary routes.
    let r = http
        .post(env.url("/v1/runs"))
        .header("authorization", &auth)
        .header("content-type", "application/json")
        .body(format!("{{\"text\": \"{}\"}}", "x".repeat(1_100_000)))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);

    // Revoking (through the daemon) signs the device out and ends its stream.
    let listing = env.ok(&["devices", "list"]);
    assert!(
        listing.contains("test phone") && listing.contains("active"),
        "{listing}"
    );
    let id = listing
        .lines()
        .find(|l| l.contains("test phone"))
        .and_then(|l| l.split_whitespace().next())
        .unwrap()
        .to_owned();
    env.ok(&["devices", "revoke", &id]);
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(_)) = events.next().await {}
    })
    .await;
    assert!(ended.is_ok(), "event stream stayed open after revocation");
    let r = http
        .get(env.url("/v1/sessions"))
        .header("authorization", &auth)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let body: Value = r.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("signed out"));
    assert!(env.ok(&["devices", "list"]).contains("revoked"));

    // Pairing attempts are rate limited per address.
    let mut statuses = Vec::new();
    for _ in 0..6 {
        let r = http
            .post(env.url("/v1/auth/pair"))
            .json(&json!({ "code": "NOPE1234" }))
            .send()
            .await
            .unwrap();
        statuses.push(r.status().as_u16());
    }
    // Two attempts were spent by `devices join` above.
    assert_eq!(statuses, [401, 401, 401, 429, 429, 429]);
}

#[test]
fn plaintext_on_a_network_is_refused() {
    let env = Env::new();
    std::fs::write(
        env.tern_home.join("config.toml"),
        "[api]\nlisten = [\"unix\", \"tcp://0.0.0.0:0\"]\ntls = \"off\"\n",
    )
    .unwrap();
    std::fs::write(&env.script, r#"{"turns": []}"#).unwrap();
    let out = env.cmd(&["daemon"]).output().unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("tls = \"off\" is only allowed"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
