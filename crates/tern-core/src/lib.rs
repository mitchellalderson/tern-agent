//! Domain types, configuration, and (from M1) the agent loop and step journal.

pub mod config;
pub mod home;
pub mod secrets;

pub use config::{Config, ConfigError};
pub use home::TernHome;
pub use secrets::{SecretError, Secrets};

/// Default contents written by `tern config init`.
pub mod defaults {
    pub const PERSONA_MD: &str = include_str!("../assets/persona.md");
    pub const USER_MD: &str = include_str!("../assets/user.md");
    pub const CONFIG_TOML: &str = include_str!("../assets/config.example.toml");
}
