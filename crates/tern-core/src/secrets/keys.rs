//! Helpers for API keys typed or pasted by a person: cleaning up the paste,
//! showing it masked, and naming the secret it is stored under.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("the key is empty")]
    Empty,
    #[error("that looks like more than one key; paste just the key")]
    Spaces,
}

/// Clean up a pasted key: trim whitespace, drop surrounding quotes or
/// backticks, and strip `export NAME=`, `NAME=` and `Bearer ` prefixes.
pub fn normalize_key(raw: &str) -> Result<String, KeyError> {
    let mut s = raw.trim();
    if let Some(rest) = s.strip_prefix("export ") {
        s = rest.trim_start();
    }
    // Only an env-style name (`OPENAI_API_KEY=`) counts as a prefix, so keys
    // with `=` in them (base64 padding) are left alone.
    if let Some((name, value)) = s.split_once('=')
        && is_env_name(name.trim())
        && !value.trim().is_empty()
    {
        s = value.trim();
    }
    s = unquote(s);
    if let Some(rest) = s
        .strip_prefix("Bearer ")
        .or_else(|| s.strip_prefix("bearer "))
    {
        s = rest.trim_start();
    }
    s = unquote(s.trim());
    if s.is_empty() {
        return Err(KeyError::Empty);
    }
    if s.chars().any(char::is_whitespace) {
        return Err(KeyError::Spaces);
    }
    Ok(s.to_owned())
}

fn is_env_name(name: &str) -> bool {
    name.contains('_')
        && super::check_name(name).is_ok()
        && !name.chars().any(|c| c.is_ascii_lowercase())
}

fn unquote(s: &str) -> &str {
    for q in ['"', '\'', '`'] {
        if let Some(inner) = s.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return inner.trim();
        }
    }
    s
}

/// `sk-ant-…a1B2`: enough to recognise a key without revealing it. Short
/// keys show only their last characters, and very short ones nothing.
pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    match chars.len() {
        0..=7 => "…".into(),
        8..=15 => format!("…{}", tail(&chars, 2)),
        _ => format!(
            "{}…{}",
            chars[..6].iter().collect::<String>(),
            tail(&chars, 4)
        ),
    }
}

fn tail(chars: &[char], n: usize) -> String {
    chars[chars.len() - n..].iter().collect()
}

/// The secret a provider's key is stored under: `ANTHROPIC_API_KEY` for a
/// provider named `anthropic`, `MY_PROXY_API_KEY` for `my-proxy`.
pub fn secret_name_for(provider: &str) -> String {
    let mut name: String = provider
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, '_');
    }
    name.push_str("_API_KEY");
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_pastes() {
        for raw in [
            "sk-ant-abc123",
            "  sk-ant-abc123\n",
            "\"sk-ant-abc123\"",
            "'sk-ant-abc123'",
            "`sk-ant-abc123`",
            "export ANTHROPIC_API_KEY=sk-ant-abc123",
            "ANTHROPIC_API_KEY=\"sk-ant-abc123\"",
            "ANTHROPIC_API_KEY = sk-ant-abc123",
            "Bearer sk-ant-abc123",
        ] {
            assert_eq!(
                normalize_key(raw).as_deref(),
                Ok("sk-ant-abc123"),
                "{raw:?}"
            );
        }
        // `=` inside a key that isn't a NAME= prefix stays.
        assert_eq!(normalize_key("abc=def").as_deref(), Ok("abc=def"));
        assert_eq!(normalize_key("a-b=c").as_deref(), Ok("a-b=c"));
        assert_eq!(normalize_key("dGVzdA==").as_deref(), Ok("dGVzdA=="));
        assert_eq!(normalize_key("MY_KEY=").as_deref(), Ok("MY_KEY="));
        assert_eq!(normalize_key("  "), Err(KeyError::Empty));
        assert_eq!(normalize_key("\"\""), Err(KeyError::Empty));
        assert_eq!(normalize_key("sk-1 sk-2"), Err(KeyError::Spaces));
    }

    #[test]
    fn masks() {
        assert_eq!(mask_key("sk-ant-api03-xxxxxxxxa1B2"), "sk-ant…a1B2");
        assert_eq!(mask_key("abcdefghij"), "…ij");
        assert_eq!(mask_key("short"), "…");
    }

    #[test]
    fn secret_names() {
        assert_eq!(secret_name_for("anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(secret_name_for("my-proxy"), "MY_PROXY_API_KEY");
        assert_eq!(secret_name_for("1min"), "_1MIN_API_KEY");
        super::super::check_name(&secret_name_for("a.b-c")).unwrap();
    }
}
