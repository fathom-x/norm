//! OAuth 2.0 PKCE helpers (RFC 7636).
//!
//! Mirrors the verifier/challenge/state generation in `wallet_mcp/cli.py` and
//! `wallet_mcp/server.py:746-826`. We use the `S256` challenge method
//! exclusively — `plain` is rejected by modern OAuth servers.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Length of the PKCE code_verifier in bytes (before base64url encoding).
/// 32 bytes → 43 chars of base64url, well within RFC 7636's 43-128 range.
const VERIFIER_BYTES: usize = 32;

/// Length of the OAuth `state` parameter in bytes (before base64url encoding).
const STATE_BYTES: usize = 16;

/// One PKCE session: the verifier (kept secret), the challenge (sent to the
/// authorization server), and an opaque CSRF-style state.
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}

impl Pkce {
    /// Generate a fresh PKCE pair plus state, all base64url-encoded.
    #[must_use]
    pub fn generate() -> Self {
        let mut v_bytes = [0u8; VERIFIER_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut v_bytes);
        let verifier = URL_SAFE_NO_PAD.encode(v_bytes);

        // challenge = base64url(sha256(verifier_ascii))
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());

        let mut s_bytes = [0u8; STATE_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut s_bytes);
        let state = URL_SAFE_NO_PAD.encode(s_bytes);

        Self {
            verifier,
            challenge,
            state,
        }
    }

    /// The challenge method to send in the OAuth `code_challenge_method` query
    /// parameter. Always `S256`.
    #[must_use]
    pub fn method() -> &'static str {
        "S256"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_sha256_of_verifier() {
        let p = Pkce::generate();
        let mut hasher = Sha256::new();
        hasher.update(p.verifier.as_bytes());
        let expected = URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(p.challenge, expected);
    }

    #[test]
    fn two_pkces_differ() {
        let a = Pkce::generate();
        let b = Pkce::generate();
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.state, b.state);
    }

    #[test]
    fn verifier_length_is_within_rfc_range() {
        let p = Pkce::generate();
        // base64url(32 bytes) without padding = 43 chars
        assert_eq!(p.verifier.len(), 43);
        assert!(p
            .verifier
            .chars()
            .all(|c| { c.is_ascii_alphanumeric() || c == '-' || c == '_' }));
    }

    /// Spec example from RFC 7636 §4.6.
    #[test]
    fn rfc7636_known_answer() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let challenge = URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }
}
