//! Errors from signing in. None of them carries a token: messages are built
//! from codes and descriptions the server sent, cut short and scrubbed of
//! the values the call had in hand.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    /// tern won't send this request (scheme, private address, bad URL).
    #[error("{0}")]
    Refused(String),
    /// The network failed.
    #[error("{0}")]
    Http(String),
    /// The server answered with an OAuth error.
    #[error("{}", oauth_message(.error, .description))]
    OAuth {
        error: String,
        description: Option<String>,
    },
    /// The grant is gone (revoked, expired, or no refresh token): the user
    /// has to sign in again.
    #[error("needs sign-in again: {0}")]
    NeedsReauth(String),
    /// The user said no at the provider.
    #[error("the sign-in was declined at the provider")]
    Denied,
    #[error("the sign-in timed out")]
    TimedOut,
    #[error("the sign-in was cancelled")]
    Cancelled,
    /// A server sent something unusable (bad metadata, no access token).
    #[error("{0}")]
    Invalid(String),
}

fn oauth_message(error: &str, description: &Option<String>) -> String {
    match description {
        Some(d) => format!("the server said {error}: {d}"),
        None => format!("the server said {error}"),
    }
}

/// At most `n` characters of server-supplied text, on one line.
pub(crate) fn short(s: &str, n: usize) -> String {
    let one: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let one = one.trim();
    if one.chars().count() <= n {
        one.to_owned()
    } else {
        format!("{}…", one.chars().take(n).collect::<String>())
    }
}
