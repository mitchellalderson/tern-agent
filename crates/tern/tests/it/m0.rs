//! M0 acceptance: doctor on a fresh machine, config init/get/set.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Command, Output};

/// The binary under test: `TERN_TEST_BIN` (e.g. a `--profile dist` build,
/// whose settings can differ from test builds) or this crate's build.
fn tern_bin() -> std::path::PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

struct Env {
    _tmp: tempfile::TempDir,
    home: std::path::PathBuf,
    tern_home: std::path::PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let tern_home = home.join(".tern");
        Self {
            _tmp: tmp,
            home,
            tern_home,
        }
    }

    fn tern(&self, args: &[&str]) -> Output {
        Command::new(tern_bin())
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TERN_HOME", &self.tern_home)
            .output()
            .unwrap()
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn doctor_on_fresh_machine_explains_setup_without_creating_anything() {
    let env = Env::new();
    let out = env.tern(&["doctor", "--offline"]);
    let text = stdout(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("tern config init"), "{text}");
    assert!(text.contains("no providers configured"), "{text}");
    assert!(text.contains("default_model is not set"), "{text}");
    assert!(!env.tern_home.exists(), "doctor must not create TERN_HOME");
}

#[test]
fn config_set_and_get_roundtrip() {
    let env = Env::new();
    assert!(env.tern(&["config", "init"]).status.success());
    let set = |k: &str, v: &str| env.tern(&["config", "set", k, v]);
    assert!(set("max_iterations", "12").status.success());
    assert!(set("default_model", "ollama:qwen3:8b").status.success());
    assert!(
        set("providers.ollama.kind", "openai_compat")
            .status
            .success()
    );
    assert!(
        set("providers.ollama.base_url", "http://localhost:11434/v1")
            .status
            .success()
    );

    let get = |k: &str| stdout(&env.tern(&["config", "get", k])).trim().to_owned();
    assert_eq!(get("max_iterations"), "12");
    assert_eq!(get("default_model"), "ollama:qwen3:8b");
    assert_eq!(get("providers.ollama.kind"), "openai_compat");

    // Comments from the shipped example survive edits.
    let text = std::fs::read_to_string(env.tern_home.join("config.toml")).unwrap();
    assert!(text.contains("# Secrets (API keys, tokens)"), "{text}");

    let doctor = stdout(&env.tern(&["doctor", "--offline"]));
    assert!(doctor.contains("default_model ollama:qwen3:8b"), "{doctor}");
}

#[test]
fn config_set_refuses_invalid_values() {
    let env = Env::new();
    assert!(env.tern(&["config", "init"]).status.success());
    let out = env.tern(&["config", "set", "max_iterations", "lots"]);
    assert!(!out.status.success());
    assert!(
        env.tern(&["config", "set", "no_such_key", "1"])
            .status
            .code()
            != Some(0)
    );
    let get = stdout(&env.tern(&["config", "get", "max_iterations"]));
    assert_eq!(get.trim(), "40");
}

#[cfg(unix)]
#[test]
fn doctor_fails_on_world_readable_secrets() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    assert!(env.tern(&["config", "init"]).status.success());
    let p = env.tern_home.join("secrets.toml");
    std::fs::write(&p, "X = \"y\"\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = env.tern(&["doctor", "--offline"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).contains("chmod 600"), "{}", stdout(&out));
}
