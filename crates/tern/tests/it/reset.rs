//! `tern reset`: deletes the data directory, optionally keeping config and
//! secrets, and never runs unconfirmed without a terminal.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn tern_bin() -> PathBuf {
    std::env::var_os("TERN_TEST_BIN").map_or_else(|| env!("CARGO_BIN_EXE_tern").into(), Into::into)
}

fn reset(home: &Path, args: &[&str]) -> Output {
    Command::new(tern_bin())
        .arg("reset")
        .args(args)
        .env("TERN_HOME", home)
        .env("HOME", home.parent().unwrap())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn populated() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join(".tern");
    std::fs::create_dir_all(home.join("logs")).unwrap();
    std::fs::create_dir_all(home.join("skills/x")).unwrap();
    for f in [
        "config.toml",
        "secrets.toml",
        "tern.db",
        "logs/a.log",
        "skills/x/SKILL.md",
    ] {
        std::fs::write(home.join(f), "x").unwrap();
    }
    (tmp, home)
}

#[test]
fn refuses_without_confirmation_off_a_terminal() {
    let (_tmp, home) = populated();
    let out = reset(&home, &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--yes"));
    assert!(home.join("tern.db").exists());
}

#[test]
fn deletes_everything() {
    let (_tmp, home) = populated();
    let out = reset(&home, &["--yes"]);
    assert!(out.status.success(), "{out:?}");
    assert!(!home.exists());
    // A second reset has nothing to do.
    assert!(reset(&home, &["--yes"]).status.success());
}

#[test]
fn keep_config_keeps_config_and_secrets() {
    let (_tmp, home) = populated();
    let out = reset(&home, &["--yes", "--keep-config"]);
    assert!(out.status.success(), "{out:?}");
    let mut left: Vec<_> = std::fs::read_dir(&home)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, ["config.toml", "secrets.toml"]);
}

#[test]
fn refuses_to_delete_the_home_directory() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("keep"), "x").unwrap();
    let out = Command::new(tern_bin())
        .args(["reset", "--yes"])
        .env("TERN_HOME", tmp.path())
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(tmp.path().join("keep").exists());
}
