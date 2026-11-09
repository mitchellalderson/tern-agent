//! The shell tool's policy for code auto mode, over every sandbox state:
//! only a verified, offline sandbox and a command the classifier clears is
//! `auto_code`; everything else is what `supervised` does.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use tern_core::approval::{Autonomy, Check, Gate, check};
use tern_core::tool::{Reach, Tool, ToolCtx};
use tern_core::workspace::{ShellBound, WorkspaceScope};
use tern_sandbox::{ExecOpts, ExecOutput, SandboxBackend, SandboxError, Verification};
use tern_tools::shell::Shell;
use tokio_util::sync::CancellationToken;

/// A backend that claims a sandbox state and runs nothing.
struct Claim {
    verification: Verification,
    network: bool,
    container: bool,
    unconfined: bool,
}

#[async_trait]
impl SandboxBackend for Claim {
    async fn exec(
        &self,
        _: &str,
        _: ExecOpts,
        _: CancellationToken,
    ) -> Result<ExecOutput, SandboxError> {
        Ok(ExecOutput::default())
    }
    fn network_reachable(&self) -> bool {
        self.network
    }
    fn is_container(&self) -> bool {
        self.container
    }
    fn unconfined(&self) -> bool {
        self.unconfined
    }
    fn verify(&self, _: &Path) -> Verification {
        self.verification.clone()
    }
}

fn verified() -> Claim {
    Claim {
        verification: Verification::Verified("test sandbox".into()),
        network: false,
        container: false,
        unconfined: false,
    }
}

fn ctx(dir: &Path) -> ToolCtx {
    let scope = WorkspaceScope::for_dir(dir.to_path_buf());
    scope.set_code_mode(true);
    ToolCtx {
        scope,
        run_id: "r".into(),
        session_id: "s".into(),
        cancel: CancellationToken::new(),
    }
}

fn verdict(claim: Claim, command: &str, network_arg: bool) -> (Check, Check, ShellBound, String) {
    let tmp = tempfile::tempdir().unwrap();
    let c = ctx(tmp.path());
    let shell = Shell::new(Arc::new(claim), Duration::from_secs(5));
    let mut args = json!({ "command": command });
    if network_arg {
        args["network"] = json!(true);
    }
    let p = shell.policy(&c, &args);
    let gate = |autonomy| Gate {
        tainted: false,
        autonomy,
    };
    (
        check("shell", &p, false, gate(Autonomy::Code)),
        check("shell", &p, false, gate(Autonomy::Supervised)),
        c.scope.shell_bound(),
        format!("reach {:?} auto_code {}", p.reach, p.auto_code),
    )
}

#[test]
fn a_verified_offline_sandbox_runs_cleared_commands_unasked() {
    for cmd in ["cargo test", "npm test", "git commit -m x", "make test"] {
        let (code, sup, bound, why) = verdict(verified(), cmd, false);
        assert_eq!(code, Check::Allow, "{cmd}: {why}");
        assert!(matches!(sup, Check::Ask { .. }), "{cmd}: supervised asks");
        assert_eq!(bound, ShellBound::Bounded);
    }
    let tmp = tempfile::tempdir().unwrap();
    let shell = Shell::new(Arc::new(verified()), Duration::from_secs(5));
    assert_eq!(
        shell
            .policy(&ctx(tmp.path()), &json!({"command": "cargo test"}))
            .reach,
        Reach::Workspace
    );
}

#[test]
fn commands_on_the_ask_list_ask_even_in_a_verified_sandbox() {
    for cmd in [
        "git push",
        "npm install",
        "pip install x",
        "curl https://x",
        "rm -rf ~",
        "sh -c 'git push'",
        "cargo test $(id)",
    ] {
        let (code, sup, _, why) = verdict(verified(), cmd, false);
        assert_eq!(code, sup, "{cmd}: {why}");
        assert!(matches!(code, Check::Ask { .. }), "{cmd}: {why}");
    }
}

#[test]
fn network_asks_every_time() {
    // A command that asks for network.
    let (code, _, _, why) = verdict(verified(), "cargo test", true);
    assert!(matches!(code, Check::Ask { .. }), "{why}");
    // A sandbox whose policy lets every command reach the network.
    let mut open = verified();
    open.network = true;
    let (code, _, _, why) = verdict(open, "cargo test", false);
    assert!(matches!(code, Check::Ask { .. }), "{why}");
}

#[test]
fn an_unverified_sandbox_asks_exactly_as_supervised() {
    // Still checking, or a failed check, or Docker unprobed.
    let states = [
        Claim {
            verification: Verification::not_verified("still checking"),
            network: false,
            container: false,
            unconfined: false,
        },
        Claim {
            verification: Verification::not_verified("a write outside the folder worked"),
            network: false,
            container: false,
            unconfined: false,
        },
        Claim {
            verification: Verification::not_verified("can't be probed"),
            network: false,
            container: true,
            unconfined: false,
        },
        // No sandbox at all (`confine = "off"` or unavailable).
        Claim {
            verification: Verification::not_verified("confinement is off"),
            network: true,
            container: false,
            unconfined: true,
        },
    ];
    for (i, s) in states.into_iter().enumerate() {
        let (code, sup, bound, why) = verdict(s, "cargo test", false);
        assert_eq!(code, sup, "state {i}: {why}");
        assert!(matches!(code, Check::Ask { .. }), "state {i}: {why}");
        assert_ne!(bound, ShellBound::Bounded, "state {i}");
    }
}

#[test]
fn the_card_says_why_a_command_asks_in_code_mode_only() {
    let tmp = tempfile::tempdir().unwrap();
    let c = ctx(tmp.path());
    let shell = Shell::new(Arc::new(verified()), Duration::from_secs(5));
    let args = json!({"command": "git push origin main"});
    shell.policy(&c, &args);
    let call = tern_core::types::ToolCall {
        id: "c".into(),
        name: "shell".into(),
        input: args.clone(),
    };
    let mut reg = tern_core::tool::ToolRegistry::new();
    reg.register(Arc::new(Shell::new(
        Arc::new(verified()),
        Duration::from_secs(5),
    )));
    reg.policy(&c, &call);
    let note = reg.grant_unavailable(&c.run_id, &call).unwrap_or_default();
    assert!(note.starts_with("Asked because: "), "{note:?}");

    // Outside code mode the card says nothing new.
    let plain = ctx(tmp.path());
    plain.scope.set_code_mode(false);
    let call2 = tern_core::types::ToolCall {
        id: "c2".into(),
        name: "shell".into(),
        input: args,
    };
    reg.policy(&plain, &call2);
    assert!(reg.grant_unavailable(&plain.run_id, &call2).is_none());
}
