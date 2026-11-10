//! Account sign-in on the TCP listener (deployment spec §10.4a): a mock JWKS
//! and self-signed ES256 test tokens, no Clerk. Valid owner, wrong owner,
//! expired, wrong issuer, wrong audience, and accounts off.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, ResponseTemplate};

const ISSUER: &str = "https://clerk.test";
const AUDIENCE: &str = "tern_client_test";
const OWNER: &str = "user_owner";
const KID: &str = "test-key";

fn tern_bin() -> PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn b64url(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = c.iter().fold(0u32, |n, b| n << 8 | u32::from(*b)) << (8 * (3 - c.len()));
        for i in 0..=c.len() {
            out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// A fresh signing key and the JWKS that publishes it.
struct Keys {
    enc: EncodingKey,
    jwks: Value,
}

fn keys() -> Keys {
    let pkcs8 =
        EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &SystemRandom::new())
            .unwrap();
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref()).unwrap();
    let point = pair.public_key().as_ref(); // 0x04 || x || y
    Keys {
        enc: EncodingKey::from_ec_der(pkcs8.as_ref()),
        jwks: json!({ "keys": [{
            "kty": "EC", "crv": "P-256", "alg": "ES256", "use": "sig", "kid": KID,
            "x": b64url(&point[1..33]), "y": b64url(&point[33..65]),
        }]}),
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn token(k: &Keys, sub: &str, iss: &str, aud: &str, exp: i64) -> String {
    let mut h = Header::new(Algorithm::ES256);
    h.kid = Some(KID.into());
    let claims =
        json!({ "sub": sub, "email": "owner@example.com", "iss": iss, "aud": aud, "exp": exp });
    encode(&h, &claims, &k.enc).unwrap()
}

struct Env {
    _tmp: tempfile::TempDir,
    tern_home: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let tern_home = tmp.path().join("home/.tern");
        std::fs::create_dir_all(&tern_home).unwrap();
        std::fs::write(tmp.path().join("script.json"), r#"{"turns":[]}"#).unwrap();
        Self {
            _tmp: tmp,
            tern_home,
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(tern_bin());
        c.args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.tern_home.parent().unwrap())
            .env("TERN_HOME", &self.tern_home)
            .env(
                "TERN_PROVIDER",
                format!("mock:{}", self._tmp.path().join("script.json").display()),
            )
            .stdin(Stdio::null());
        c
    }

    fn out(&self, args: &[&str]) -> (bool, String) {
        let out = self.cmd(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    /// Start a daemon with a TCP listener and `account` appended to its
    /// config; returns its port once it answers over TLS.
    async fn daemon(&self, account: &str, http: &reqwest::Client) -> (u16, Killed) {
        let socket = self.tern_home.join("tern.sock");
        for _ in 0..5 {
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            std::fs::write(
                self.tern_home.join("config.toml"),
                format!(
                    "[api]\nlisten = [\"unix\", \"tcp://127.0.0.1:{port}\"]\ntls = \"auto\"\n{account}"
                ),
            )
            .unwrap();
            let _ = std::fs::remove_file(&socket);
            let mut daemon = Killed(
                self.cmd(&["daemon"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                if daemon.0.try_wait().unwrap().is_some() {
                    break; // port taken; try another
                }
                let r = http
                    .post(format!("https://127.0.0.1:{port}/v1/shutdown"))
                    .send()
                    .await;
                if matches!(&r, Ok(r) if r.status() == 401) {
                    return (port, daemon);
                }
                assert!(Instant::now() < end, "tcp listener did not start");
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        panic!("no free port after 5 tries");
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .build()
        .unwrap()
}

async fn status_of(http: &reqwest::Client, port: u16, path: &str, token: &str) -> u16 {
    http.get(format!("https://127.0.0.1:{port}{path}"))
        .header("authorization", format!("Bearer {token}"))
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_owners_valid_account_token_is_accepted() {
    let k = keys();
    let jwks = MockServer::start().await;
    Mock::given(path("/jwks.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&k.jwks))
        .mount(&jwks)
        .await;
    let env = Env::new();
    let http = client();
    let account = format!(
        "[account]\nissuer = \"{ISSUER}\"\naudience = \"{AUDIENCE}\"\nowner = \"{OWNER}\"\njwks_url = \"{}/jwks.json\"\n",
        jwks.uri()
    );
    let (port, _daemon) = env.daemon(&account, &http).await;
    let soon = now() + 600;

    // The owner, on an owner-only route.
    let good = token(&k, OWNER, ISSUER, AUDIENCE, soon);
    assert_eq!(status_of(&http, port, "/v1/devices", &good).await, 200);
    let health: Value = http
        .get(format!("https://127.0.0.1:{port}/v1/health"))
        .header("authorization", format!("Bearer {good}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["auth"]["kind"], "account");

    // Another account with a perfectly valid token.
    let other = token(&k, "user_other", ISSUER, AUDIENCE, soon);
    assert_eq!(status_of(&http, port, "/v1/health", &other).await, 403);
    // Expired (past the 30 s leeway), wrong issuer, wrong audience.
    for (why, t) in [
        ("expired", token(&k, OWNER, ISSUER, AUDIENCE, now() - 3600)),
        (
            "issuer",
            token(&k, OWNER, "https://evil.test", AUDIENCE, soon),
        ),
        ("audience", token(&k, OWNER, ISSUER, "someone_else", soon)),
        ("signature", token(&keys(), OWNER, ISSUER, AUDIENCE, soon)),
    ] {
        assert_eq!(status_of(&http, port, "/v1/health", &t).await, 401, "{why}");
    }
    assert_eq!(status_of(&http, port, "/v1/health", "not.a.jwt").await, 401);

    // The local socket is untouched: no token, still the owner.
    let local = reqwest::Client::builder()
        .unix_socket(env.tern_home.join("tern.sock"))
        .build()
        .unwrap();
    let health: Value = local
        .get("http://tern/v1/health")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["auth"]["kind"], "owner");

    // `tern account status` reaches the mock keys.
    let (ok, text) = env.out(&["account", "status"]);
    assert!(ok, "{text}");
    assert!(text.contains("Accounts: on") && text.contains(OWNER) && text.contains("1 fetched"));
}

#[tokio::test(flavor = "multi_thread")]
async fn no_owner_means_no_account_gets_in() {
    let k = keys();
    let jwks = MockServer::start().await;
    Mock::given(path("/jwks.json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&k.jwks))
        .mount(&jwks)
        .await;
    let env = Env::new();
    let http = client();
    let account = format!(
        "[account]\nissuer = \"{ISSUER}\"\naudience = \"{AUDIENCE}\"\njwks_url = \"{}/jwks.json\"\n",
        jwks.uri()
    );
    let (port, _daemon) = env.daemon(&account, &http).await;
    let t = token(&k, OWNER, ISSUER, AUDIENCE, now() + 600);
    assert_eq!(status_of(&http, port, "/v1/health", &t).await, 403);
}

#[tokio::test(flavor = "multi_thread")]
async fn accounts_off_changes_nothing() {
    let k = keys();
    let env = Env::new();
    let http = client();
    let (port, _daemon) = env.daemon("", &http).await;
    // A JWT is just a bad device token when accounts are off.
    let t = token(&k, OWNER, ISSUER, AUDIENCE, now() + 600);
    assert_eq!(status_of(&http, port, "/v1/health", &t).await, 401);
    let (ok, text) = env.out(&["account", "status"]);
    assert!(ok && text.contains("Accounts: off"), "{text}");
}
