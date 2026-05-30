//! Password-based key derivation.
//!
//! Mirrors `wallet_mcp/db.py:175-213` exactly. The verify hash uses count=1
//! (intentionally cheap — comparison is constant-time and the real work
//! factor lives in the 600k-iteration key derivation).

use hmac::Hmac;
use pbkdf2::pbkdf2;
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub const KEY_LEN: usize = 32;
pub const SALT_LEN: usize = 32;
pub const KDF_ITERATIONS: u32 = 600_000;

/// Derive a 32-byte AES key from a password and 32-byte salt.
///
/// Equivalent to `PBKDF2(password, salt, dkLen=32, count=600_000, hmac_hash_module=SHA256)`.
#[must_use]
pub fn derive_key(password: &str, salt: &[u8]) -> [u8; KEY_LEN] {
    let mut out = [0u8; KEY_LEN];
    pbkdf2::<Hmac<Sha256>>(password.as_bytes(), salt, KDF_ITERATIONS, &mut out)
        .expect("PBKDF2-SHA256 with non-zero iters never fails");
    out
}

/// Compute the lowercase-hex verify hash stored in `settings.password_hash`.
///
/// Equivalent to `PBKDF2(password, salt, dkLen=32, count=1, hmac_hash_module=SHA256).hex()`.
#[must_use]
pub fn verify_hash(password: &str, salt: &[u8]) -> String {
    let mut out = [0u8; KEY_LEN];
    pbkdf2::<Hmac<Sha256>>(password.as_bytes(), salt, 1, &mut out)
        .expect("PBKDF2-SHA256 with non-zero iters never fails");
    hex::encode(out)
}

/// Constant-time comparison of two hex-encoded hashes.
#[must_use]
pub fn hashes_equal(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference vector computed with Python:
    //   hashlib.pbkdf2_hmac('sha256', b'password', b'\\x00'*32, 1, 32).hex()
    // PyCryptodome's PBKDF2(..., hmac_hash_module=SHA256) produces the same bytes.
    #[test]
    fn verify_hash_known_answer_zero_salt() {
        let got = verify_hash("password", &[0u8; 32]);
        assert_eq!(
            got,
            "42bc9102bad816a67df41f09cef74023de1433e58d2da3db7eb2decbd7c60ebf"
        );
    }

    #[test]
    fn derive_key_is_deterministic() {
        let salt = [7u8; 32];
        let a = derive_key("hunter2", &salt);
        let b = derive_key("hunter2", &salt);
        assert_eq!(a, b);
    }

    #[test]
    fn derive_key_differs_per_password() {
        let salt = [7u8; 32];
        assert_ne!(derive_key("a", &salt), derive_key("b", &salt));
    }

    #[test]
    fn derive_key_differs_per_salt() {
        assert_ne!(derive_key("pw", &[0u8; 32]), derive_key("pw", &[1u8; 32]));
    }

    #[test]
    fn hashes_equal_constant_time() {
        assert!(hashes_equal("abc", "abc"));
        assert!(!hashes_equal("abc", "abd"));
        assert!(!hashes_equal("abc", "abcd"));
    }
}
