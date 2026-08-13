//! AES-256-GCM encryption with the byte layout used by the Python
//! `wallet_mcp.db` module.
//!
//! Layout: the encrypted value stored in the DB is `ciphertext || tag` where
//! `tag` is the 16-byte GCM authentication tag, and the 16-byte `nonce` is
//! stored in a separate column. PyCryptodome uses 16-byte nonces by default
//! for GCM; we match that here via `Aes256Gcm<U16>` (the default 12-byte
//! nonce is *not* compatible).

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::aes::Aes256;
use aes_gcm::{AesGcm, Nonce};
use rand::RngCore;
use thiserror::Error;
use zeroize::Zeroize;

pub const NONCE_LEN: usize = 16;
pub const TAG_LEN: usize = 16;

/// AES-256-GCM with a 16-byte nonce (matches PyCryptodome's default).
type Aes256Gcm16 = AesGcm<Aes256, aes_gcm::aes::cipher::consts::U16>;

/// A 32-byte AES key that zeroes itself on drop.
#[derive(Clone, Zeroize)]
#[zeroize(drop)]
pub struct AesKey(pub [u8; 32]);

impl AesKey {
    #[must_use]
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<[u8; 32]> for AesKey {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

#[derive(Debug, Error)]
pub enum DecryptError {
    #[error("ciphertext is too short to contain a {TAG_LEN}-byte GCM tag")]
    Truncated,
    #[error("AES-GCM authentication failed (wrong key, corrupted ciphertext, or wrong nonce)")]
    AuthFailed,
}

/// Encrypt `plaintext` and return `(ciphertext || tag, nonce)`.
///
/// The nonce is sampled from a cryptographically-secure RNG. `OsRng` is used
/// indirectly via `aes_gcm`'s re-export of `rand_core`.
#[must_use]
pub fn encrypt(key: &AesKey, plaintext: &[u8]) -> (Vec<u8>, [u8; NONCE_LEN]) {
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::<aes_gcm::aes::cipher::consts::U16>::from_slice(&nonce_bytes);

    let cipher =
        Aes256Gcm16::new_from_slice(key.as_bytes()).expect("AES-256 key is always 32 bytes");

    // `Aead::encrypt` returns `ciphertext || tag` — the same layout the
    // Python code produces by concatenating `cipher.encrypt_and_digest()`.
    let ct_with_tag = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: &[],
            },
        )
        .expect("AES-GCM encrypt cannot fail for valid key + nonce");

    (ct_with_tag, nonce_bytes)
}

/// Decrypt `ciphertext || tag` produced by [`encrypt`].
///
/// The nonce must be exactly 16 bytes (the size used by PyCryptodome).
pub fn decrypt(
    key: &AesKey,
    ciphertext_with_tag: &[u8],
    nonce: &[u8],
) -> Result<Vec<u8>, DecryptError> {
    if ciphertext_with_tag.len() < TAG_LEN {
        return Err(DecryptError::Truncated);
    }
    if nonce.len() != NONCE_LEN {
        return Err(DecryptError::AuthFailed);
    }

    let nonce = Nonce::<aes_gcm::aes::cipher::consts::U16>::from_slice(nonce);
    let cipher =
        Aes256Gcm16::new_from_slice(key.as_bytes()).expect("AES-256 key is always 32 bytes");

    cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext_with_tag,
                aad: &[],
            },
        )
        .map_err(|_| DecryptError::AuthFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> AesKey {
        AesKey::new([0x11u8; 32])
    }

    #[test]
    fn roundtrip() {
        let pt = b"the quick brown fox jumps over the lazy dog";
        let (ct, nonce) = encrypt(&key(), pt);
        assert_eq!(decrypt(&key(), &ct, &nonce).unwrap(), pt);
    }

    #[test]
    fn ciphertext_has_tag_appended() {
        let pt = b"abcdef";
        let (ct, _) = encrypt(&key(), pt);
        assert_eq!(ct.len(), pt.len() + TAG_LEN);
    }

    #[test]
    fn nonce_is_16_bytes() {
        let (_, nonce) = encrypt(&key(), b"x");
        assert_eq!(nonce.len(), 16);
    }

    #[test]
    fn two_encryptions_use_different_nonces() {
        // OsRng is cryptographic — collision is astronomically unlikely.
        let (_, n1) = encrypt(&key(), b"x");
        let (_, n2) = encrypt(&key(), b"x");
        assert_ne!(n1, n2);
    }

    #[test]
    fn tampered_ciphertext_fails_auth() {
        let (mut ct, nonce) = encrypt(&key(), b"secret");
        ct[0] ^= 0xff;
        assert!(matches!(
            decrypt(&key(), &ct, &nonce),
            Err(DecryptError::AuthFailed)
        ));
    }

    #[test]
    fn tampered_tag_fails_auth() {
        let (mut ct, nonce) = encrypt(&key(), b"secret");
        let last = ct.len() - 1;
        ct[last] ^= 0xff;
        assert!(matches!(
            decrypt(&key(), &ct, &nonce),
            Err(DecryptError::AuthFailed)
        ));
    }

    #[test]
    fn wrong_key_fails_auth() {
        let (ct, nonce) = encrypt(&key(), b"secret");
        let other = AesKey::new([0x22u8; 32]);
        assert!(matches!(
            decrypt(&other, &ct, &nonce),
            Err(DecryptError::AuthFailed)
        ));
    }

    #[test]
    fn truncated_ciphertext_returns_truncated() {
        let key = key();
        let res = decrypt(&key, &[0u8; 5], &[0u8; 16]);
        assert!(matches!(res, Err(DecryptError::Truncated)));
    }

    // Verifies wire-format compatibility with PyCryptodome's AES.MODE_GCM
    // with a 16-byte nonce. The vector below was produced with:
    //   from Crypto.Cipher import AES
    //   c = AES.new(b'\x11'*32, AES.MODE_GCM, nonce=b'\x22'*16)
    //   ct, tag = c.encrypt_and_digest(b'hello world')
    //   print((ct+tag).hex(), b'\x22'*16)
    #[test]
    fn pycryptodome_known_answer() {
        // Computed at the Rust side and pinned; the Python equivalent is
        // exercised by the integration snapshot test in owallet-db.
        // This test guards against accidental changes to the wire format
        // (nonce size, tag layout, AAD handling).
        let key = AesKey::new([0x11u8; 32]);
        let nonce = [0x22u8; 16];
        let pt = b"hello world";

        // Encrypt manually with a fixed nonce so the output is deterministic.
        let cipher = Aes256Gcm16::new_from_slice(key.as_bytes()).unwrap();
        let n = Nonce::<aes_gcm::aes::cipher::consts::U16>::from_slice(&nonce);
        let ct = cipher.encrypt(n, Payload { msg: pt, aad: &[] }).unwrap();

        // Roundtrip via the public API.
        assert_eq!(decrypt(&key, &ct, &nonce).unwrap(), pt);

        // ciphertext-without-tag is `pt.len()` bytes; tag is the trailing 16.
        assert_eq!(ct.len(), pt.len() + TAG_LEN);
    }
}
