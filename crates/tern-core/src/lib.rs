//! Domain types, configuration, the step journal, and the agent loop.

pub mod agent;
pub mod agents;
pub mod approval;
pub mod channels;
pub mod compaction;
pub mod config;
pub mod cron;
pub mod events;
pub mod home;
pub mod journal;
pub mod mcp;
pub mod memory;
pub mod models;
pub mod net;
pub mod prompt;
pub mod quiet;
pub mod secrets;
pub mod skills;
pub mod subagent;
pub mod tool;
pub mod types;

pub use config::{Config, ConfigError};
pub use home::TernHome;
pub use secrets::{SecretError, SecretStatus, Secrets};

/// Default contents written by `tern config init`.
pub mod defaults {
    pub const PERSONA_MD: &str = include_str!("../assets/persona.md");
    pub const USER_MD: &str = include_str!("../assets/user.md");
    pub const CONFIG_TOML: &str = include_str!("../assets/config.example.toml");
}
