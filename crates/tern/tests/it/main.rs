//! One integration-test binary: every file here is a module, so the crate
//! links once instead of once per file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_admin;
mod ap0;
mod ap2;
mod ap3;
mod bundles;
mod c5;
mod code_auto;
mod config_api;
mod connections;
mod delegation;
mod documents;
mod edit_delete;
mod g2;
mod grants_e2e;
mod in_place;
mod m0;
mod m1;
mod m2;
mod m4;
mod m5;
mod m6;
mod mcp_api;
mod mcp_cli;
mod memory_class;
mod new_defaults;
mod notes;
mod proactive_delegation;
mod providers;
mod py0;
mod reset;
mod role_infer;
mod routines_visible;
mod run_reliability;
mod s1;
mod s2;
mod s3;
mod sandbox;
mod script_grants_e2e;
mod secret_requests;
mod seed_agents;
mod session_pages;
mod starters;
mod subagents_env;
mod t1a;
mod t1b;
mod t2;
mod t3;
mod t3a;
mod t4c;
mod t5b;
mod t6a;
mod t6b;
mod t6d;
mod t8a;
mod wait_until;
mod workspaces;

/// The secrets in `home` as `NAME = "value"` lines. The file itself holds
/// them encrypted, so tests read them through tern's own loader.
pub fn read_secrets(home: &std::path::Path) -> String {
    let Ok(s) = tern_core::Secrets::load(&home.join("secrets.toml")) else {
        return String::new();
    };
    s.file_names()
        .iter()
        .filter_map(|n| {
            let key = if n.contains('/') {
                format!("{n:?}")
            } else {
                n.clone()
            };
            Some(format!("{key} = {:?}\n", s.get(n)?))
        })
        .collect()
}
