//! Signing in to services (tooling spec §6.2, §6.7–6.8): the OAuth helper,
//! token storage, and the connections (accounts) that use them.
//!
//! The flows run from a front end or the CLI, never from the model: the
//! agent asks with a card and learns only whether it worked. Tokens live in
//! secrets.toml under `conn/<target>/<slug>/oauth/` and reach a service
//! only through the host (`ctx.http`'s `auth`, an MCP client's header).

pub mod b64;
pub mod connections;
pub mod discover;
pub mod domain;
pub mod error;
pub mod flow;
pub mod http;
pub mod pkce;
pub mod profiles;
pub mod register;
pub mod signin;
pub mod tokens;
pub mod tools;

pub use connections::{Conn, ConnAuth, Connections, Kind, Resolved};
pub use discover::{AuthPlan, ClientKind, Source};
pub use error::AuthError;
pub use flow::{Client, DeviceCode, OAuthConfig, PendingSignIn, TokenAuth, TokenSet};
pub use http::Guard;
pub use tokens::{ConnKey, Tokens};

/// The account every connector and MCP server has before a second one is
/// added; it uses plain secret names (tooling spec §6.8.1, migration).
pub const DEFAULT_SLUG: &str = "default";
