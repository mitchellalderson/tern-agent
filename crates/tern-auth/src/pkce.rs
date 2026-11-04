//! PKCE (RFC 7636, S256 only) and the `state` value.

use aws_lc_rs::rand::{SecureRandom, SystemRandom};

/// `n` random bytes from the OS generator, base64url-encoded.
pub fn random(n: usize) -> String {
    let mut buf = vec![0u8; n];
    // The OS generator doesn't fail in practice; if it ever did, an
    // all-zero value would be guessable, so stop instead.
    #[allow(clippy::expect_used)]
    SystemRandom::new()
        .fill(&mut buf)
        .expect("the system random generator failed");
    crate::b64::encode(&buf)
}

/// A code verifier: 32 random bytes, 43 characters (RFC 7636 §4.1).
pub fn verifier() -> String {
    random(32)
}

/// `BASE64URL(SHA256(verifier))`.
pub fn challenge(verifier: &str) -> String {
    let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, verifier.as_bytes());
    crate::b64::encode(d.as_ref())
}

/// Compares without stopping at the first difference.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_example() {
        // Appendix B.
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let v = verifier();
        assert_eq!(v.len(), 43);
        assert_ne!(v, verifier());
        assert!(same("abc", "abc") && !same("abc", "abd") && !same("abc", "ab"));
    }
}
