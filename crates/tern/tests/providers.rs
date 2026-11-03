//! Provider setup over the daemon API (docs/provider-setup-spec.md, P0):
//! verify a key, add a provider, resolve models, run against it without a
//! restart, change the default and remove it. The provider is a local
//! OpenAI-compatible fake.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use reqwest::Method;
use serde_json::{Value, json};
use wiremock::matchers::{bearer_token, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn tern_bin() -> PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

struct Daemon {
    child: Child,
    tmp: tempfile::TempDir,
    socket: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Daemon {
    fn tern_home(&self) -> PathBuf {
        self.tmp.path().join("home/.tern")
    }
}

fn daemon() -> Daemon {
    daemon_with(&[])
}

/// A daemon with extra environment variables (it starts from a clear one).
fn daemon_with(env: &[(&str, &str)]) -> Daemon {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let tern_home = home.join(".tern");
    std::fs::create_dir_all(&tern_home).unwrap();
    let socket = tern_home.join("tern.sock");
    let child = Command::new(tern_bin())
        .arg("daemon")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("TERN_HOME", &tern_home)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(Instant::now() < end, "daemon did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    Daemon { child, tmp, socket }
}

struct Api(reqwest::Client);

impl Api {
    fn new(d: &Daemon) -> Self {
        Self(
            reqwest::Client::builder()
                .unix_socket(d.socket.clone())
                .build()
                .unwrap(),
        )
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (u16, String) {
        let mut req = self.0.request(method, format!("http://tern{path}"));
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        (status, res.text().await.unwrap())
    }

    async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> Value {
        let (status, text) = self.call(method.clone(), path, body).await;
        assert_eq!(status, 200, "{method} {path}: {text}");
        serde_json::from_str(&text).unwrap()
    }
}

const STREAM: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// An OpenAI-compatible server at `/v1` that accepts the key `sk-good`.
async fn fake_provider() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(bearer_token("sk-good"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "gpt-old", "created": 1},
                {"id": "text-embedding-3-small", "created": 3},
                {"id": "gpt-new", "created": 2}
            ]
        })))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(bearer_token("sk-good"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(STREAM, "text/event-stream"))
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn add_use_and_remove_a_provider_without_restarting() {
    let fake = fake_provider().await;
    let root = fake.uri();
    let d = daemon();
    let api = Api::new(&d);

    let info = api.ok(Method::GET, "/v1/info", None).await;
    assert_eq!(info["setup_needed"], true);

    // A messy paste of a good key, at a URL missing `/v1`.
    let v = api
        .ok(
            Method::POST,
            "/v1/providers/verify",
            Some(json!({
                "kind": "openai_compat",
                "base_url": root,
                "key": "  export OPENAI_API_KEY=\"sk-good\"\n",
            })),
        )
        .await;
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["name"], "local");
    assert_eq!(v["suggested_base_url"], format!("{root}/v1"));
    let ids: Vec<&str> = v["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["gpt-new", "gpt-old"]);

    let v = api
        .ok(
            Method::POST,
            "/v1/providers/verify",
            Some(json!({ "kind": "openai_compat", "base_url": format!("{root}/v1"), "key": "sk-bad" })),
        )
        .await;
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["kind"], "unauthorized");
    assert!(
        v["message"].as_str().unwrap().contains("rejected that key"),
        "{v}"
    );

    // A bad key is refused and nothing is written.
    let (status, text) = api
        .call(
            Method::POST,
            "/v1/providers",
            Some(json!({ "kind": "openai_compat", "name": "fake", "base_url": root, "key": "sk-bad" })),
        )
        .await;
    assert_eq!(status, 400, "{text}");
    assert!(!d.tern_home().join("config.toml").exists());

    let added = api
        .ok(
            Method::POST,
            "/v1/providers",
            Some(json!({ "kind": "openai_compat", "name": "fake", "base_url": root, "key": "sk-good" })),
        )
        .await;
    assert_eq!(added["provider"]["base_url"], format!("{root}/v1"));
    assert_eq!(added["provider"]["is_default"], true);
    assert_eq!(added["provider"]["key_source"], "file");
    assert!(
        !added.to_string().contains("sk-good"),
        "key leaked: {added}"
    );
    let secrets = std::fs::read_to_string(d.tern_home().join("secrets.toml")).unwrap();
    assert!(secrets.contains("FAKE_API_KEY = \"sk-good\""), "{secrets}");
    let config = std::fs::read_to_string(d.tern_home().join("config.toml")).unwrap();
    assert!(config.contains("[providers.fake]"), "{config}");

    let info = api.ok(Method::GET, "/v1/info", None).await;
    assert_eq!(info["setup_needed"], false);
    assert_eq!(info["default_model"], "fake:gpt-new");

    // Provider-less references resolve from the cached list.
    let v = api
        .ok(
            Method::POST,
            "/v1/models/check",
            Some(json!({ "model": "gpt-old" })),
        )
        .await;
    assert_eq!(v["resolved"], "fake:gpt-old");
    assert!(v["warning"].is_null());
    let v = api
        .ok(
            Method::POST,
            "/v1/models/check",
            Some(json!({ "model": "fake:mystery" })),
        )
        .await;
    assert!(
        v["warning"]
            .as_str()
            .unwrap()
            .contains("not in fake's model list")
    );
    let (status, _) = api
        .call(
            Method::POST,
            "/v1/models/check",
            Some(json!({ "model": "nope" })),
        )
        .await;
    assert_eq!(status, 400);

    let providers = api.ok(Method::GET, "/v1/providers", None).await;
    assert_eq!(providers[0]["name"], "fake");
    assert_eq!(providers[0]["secret"], "FAKE_API_KEY");
    let models = api.ok(Method::GET, "/v1/models", None).await;
    assert_eq!(models[0]["provider"], "fake");
    assert_eq!(models[0]["models"].as_array().unwrap().len(), 2);

    // The next run uses the new provider: no restart.
    let started = api
        .ok(Method::POST, "/v1/runs", Some(json!({ "text": "hi" })))
        .await;
    assert_eq!(started["model"], "fake:gpt-new");
    let run = started["run_id"].as_str().unwrap();
    api.ok(Method::GET, &format!("/v1/runs/{run}/wait"), None)
        .await;
    let chats: Vec<_> = fake
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/v1/chat/completions")
        .collect();
    assert_eq!(chats.len(), 1);
    let body: Value = serde_json::from_slice(&chats[0].body).unwrap();
    assert_eq!(body["model"], "gpt-new");

    let v = api
        .ok(
            Method::PUT,
            "/v1/config/default_model",
            Some(json!({ "model": "gpt-old" })),
        )
        .await;
    assert_eq!(v["model"], "fake:gpt-old");
    let info = api.ok(Method::GET, "/v1/info", None).await;
    assert_eq!(info["default_model"], "fake:gpt-old");

    api.ok(
        Method::DELETE,
        "/v1/providers/fake?remove_secret=true",
        None,
    )
    .await;
    let info = api.ok(Method::GET, "/v1/info", None).await;
    assert_eq!(info["setup_needed"], true);
    assert!(info["default_model"].is_null());
    let secrets = std::fs::read_to_string(d.tern_home().join("secrets.toml")).unwrap();
    assert!(!secrets.contains("FAKE_API_KEY"), "{secrets}");
}

#[tokio::test(flavor = "multi_thread")]
async fn anthropic_needs_a_key_and_names_itself() {
    let d = daemon();
    let api = Api::new(&d);
    let (status, text) = api
        .call(
            Method::POST,
            "/v1/providers/verify",
            Some(json!({ "kind": "openai_compat", "base_url": "http://api.example.com/v1" })),
        )
        .await;
    assert_eq!(status, 400);
    assert!(text.contains("https://"), "{text}");

    let v = api
        .ok(
            Method::POST,
            "/v1/providers/verify",
            Some(json!({ "kind": "anthropic", "base_url": "http://127.0.0.1:9" })),
        )
        .await;
    assert_eq!(v["name"], "anthropic");
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["kind"], "missing_key");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_in_the_environment_wins() {
    let fake = fake_provider().await;
    let base = format!("{}/v1", fake.uri());
    let d = daemon_with(&[("FAKE_API_KEY", "sk-good")]);
    let api = Api::new(&d);

    // No paste needed: use the key already in the environment.
    let v = api
        .ok(
            Method::POST,
            "/v1/providers/verify",
            Some(json!({ "kind": "openai_compat", "name": "fake", "base_url": base, "use_env_key": true })),
        )
        .await;
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["warnings"].as_array().unwrap().len(), 0);

    // Pasting a different key warns that the environment overrides it.
    let v = api
        .ok(
            Method::POST,
            "/v1/providers/verify",
            Some(json!({ "kind": "openai_compat", "name": "fake", "base_url": base, "key": "sk-other" })),
        )
        .await;
    let warnings = v["warnings"].to_string();
    assert!(
        warnings.contains("FAKE_API_KEY in your environment overrides"),
        "{v}"
    );

    let added = api
        .ok(
            Method::POST,
            "/v1/providers",
            Some(json!({ "kind": "openai_compat", "name": "fake", "base_url": base, "use_env_key": true })),
        )
        .await;
    assert_eq!(added["provider"]["key_source"], "env", "{added}");
    assert_eq!(added["provider"]["secret"], "FAKE_API_KEY");
    // The environment's key is used, not copied into secrets.toml.
    let secrets = std::fs::read_to_string(d.tern_home().join("secrets.toml")).unwrap_or_default();
    assert!(!secrets.contains("sk-good"), "{secrets}");
    let info = api.ok(Method::GET, "/v1/info", None).await;
    assert_eq!(info["setup_needed"], false);

    // Asking for the environment key when there is none is an error.
    let (status, text) = api
        .call(
            Method::POST,
            "/v1/providers/verify",
            Some(json!({ "kind": "openai_compat", "name": "other", "base_url": base, "use_env_key": true })),
        )
        .await;
    assert_eq!(status, 400);
    assert!(text.contains("OTHER_API_KEY is not set"), "{text}");
}

fn ids(list: &Value) -> Vec<String> {
    list[0]["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_model_lists_are_served_then_refreshed() {
    let fake = fake_provider().await;
    let base = format!("{}/v1", fake.uri());
    // Every cached list counts as stale.
    let d = daemon_with(&[("TERN_MODELS_FRESH_SECS", "0")]);
    let api = Api::new(&d);
    api.ok(
        Method::POST,
        "/v1/providers",
        Some(
            json!({ "kind": "openai_compat", "name": "fake", "base_url": base, "key": "sk-good" }),
        ),
    )
    .await;

    // The provider now lists a new model.
    fake.reset().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "gpt-newest", "created": 9}]
        })))
        .mount(&fake)
        .await;

    // The stale list comes back at once while a refresh runs behind it.
    let first = api.ok(Method::GET, "/v1/models", None).await;
    assert_eq!(ids(&first), ["gpt-new", "gpt-old"]);
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let now = api.ok(Method::GET, "/v1/models", None).await;
        if ids(&now) == ["gpt-newest"] {
            break;
        }
        assert!(Instant::now() < end, "list never refreshed: {now}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The refreshed list is saved to cache/models.json, without the key.
    let cache = std::fs::read_to_string(d.tern_home().join("cache/models.json")).unwrap();
    assert!(cache.contains("gpt-newest"), "{cache}");
    assert!(
        !cache.contains("sk-good"),
        "key leaked into the cache: {cache}"
    );

    // `refresh=true` fetches now, without serving the cached list.
    fake.reset().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"id": "gpt-latest"}]
        })))
        .mount(&fake)
        .await;
    let v = api.ok(Method::GET, "/v1/models?refresh=true", None).await;
    assert_eq!(ids(&v), ["gpt-latest"]);
}
