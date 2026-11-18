//! `/healthz` and `/v1/health`: liveness, and what this core can do.
//!
//! The API is on the Unix socket only (mode 0600): whoever can open it is
//! the owner and needs no token. tern is local-only, so there is no other
//! listener, no device pairing and no scopes.
//!
//! | Method | Path | |
//! |---|---|---|
//! | GET | `/healthz` | liveness, no details |

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use serde_json::{Value, json};

use super::manager::Manager;

type Shared = Arc<Manager>;

/// Bumped when `/v1` changes incompatibly.
pub const API_VERSION: u32 = 1;

/// What `/v1/health` advertises; clients derive behaviour from it.
pub const FEATURES: &[&str] = &[
    // `/v1/providers`, `/v1/models` and `/v1/config/default_model`.
    "providers",
    "schedules",
    "uploads",
    "subagents",
    "secret_requests",
    "mcp",
    // `/v1/channels` and Home's stream (docs/channels-design.md).
    "channels",
    // `/v1/agents`: agent profiles, each with its own DM
    // (docs/agent-profiles-design.md).
    "agents",
    // `/v1/outbox` and `outbox_changed` events (tooling spec §5.2).
    "outbox",
    // `/v1/connections`, `/v1/auth-prompts` and their events (tooling spec
    // §6.7–6.8).
    "connections",
    // `/v1/ledger`: the hash-chained activity ledger (tooling spec §8.1).
    "ledger",
    // `/v1/tasks`: everything running, queued, scheduled, waiting on you or
    // failed recently (tooling spec §8.2).
    "tasks",
    // `/v1/waits`: active `wait_until` waits, and cancelling one.
    "waits",
    // `/v1/security/audit`: `tern security audit` (tooling spec §8.4).
    "security_audit",
    // `/v1/bundles`: preview, install, remove (tooling spec §6.5).
    "bundles",
    // `/v1/skills` and `/v1/skill-offers`: skill capture (tooling spec §7, T5b).
    "skill_capture",
    // `/v1/workspaces`: the folder an agent can reach, chosen per Home topic,
    // channel and agent, with read-only extra folders
    // (docs/alpha-workspaces.md). Older cores lack it: hide the folder picker.
    "workspaces",
    // `/v1/documents`: what `create_document` and `edit_document` made, with
    // its revisions (docs/documents-design.md). Older cores lack it: hide the
    // Documents panel.
    "documents",
    // `/v1/local/*`: models on this computer, through Ollama
    // (docs/local-models.md).
    "local_models",
];

/// Routes this module adds to the API router.
pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(super::uploads::routes())
}

/// `/v1/health`: this core and what it can do.
pub fn health() -> Value {
    json!({
        "pid": std::process::id(),
        "version": env!("CARGO_PKG_VERSION"),
        "api": API_VERSION,
        "store": "sqlite",
        "library": "fs",
        "features": FEATURES,
    })
}
