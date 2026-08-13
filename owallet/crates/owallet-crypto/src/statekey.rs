//! Per-wallet state-encryption key derivation.
//!
//! Files kept under a wallet's per-`npub` state directory are encrypted with a
//! key derived from *that wallet's* secp256k1 private key — so the encrypted
//! state is bound to the wallet itself, independent of the DB password. The
//! private key is high-entropy, so a single HKDF-SHA256 pass is used here
//! rather than the slow password PBKDF2 in [`crate::kdf`].

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::aesgcm::AesKey;
use crate::hd::PrivateKey;

type HmacSha256 = Hmac<Sha256>;

/// Domain-separation salt for HKDF-extract. Versioned so the derivation can
/// be rotated in future without silently colliding with existing ciphertext.
const STATE_KEY_SALT: &[u8] = b"owallet-wallet-state-key/v1";
/// HKDF-expand `info` string (context binding).
const STATE_KEY_INFO: &[u8] = b"owallet per-wallet state encryption";

/// Derive the AES-256 key used to encrypt a wallet's per-`npub` state files
/// from its private key, via HKDF-SHA256 (RFC 5869, single 32-byte block).
///
/// Deterministic: the same private key always yields the same key, so state
/// written in one session decrypts in the next. Distinct wallets get distinct
/// keys because the private key is the only input keying material.
#[must_use]
pub fn derive_state_key(private_key: &PrivateKey) -> AesKey {
    // HKDF-extract: PRK = HMAC(salt, IKM).
    let mut mac = HmacSha256::new_from_slice(STATE_KEY_SALT).expect("HMAC accepts any key length");
    mac.update(private_key.as_bytes());
    let prk = mac.finalize().into_bytes();

    // HKDF-expand for a single 32-byte block: T(1) = HMAC(PRK, info || 0x01).
    let mut mac = HmacSha256::new_from_slice(&prk).expect("HMAC accepts any key length");
    mac.update(STATE_KEY_INFO);
    mac.update(&[0x01]);
    let okm = mac.finalize().into_bytes();

    let mut key = [0u8; 32];
    key.copy_from_slice(&okm);
    AesKey::new(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aesgcm::{decrypt, encrypt};

    fn sk(byte: u8) -> PrivateKey {
        PrivateKey([byte; 32])
    }

    #[test]
    fn deterministic_for_same_key() {
        let a = derive_state_key(&sk(0x11));
        let b = derive_state_key(&sk(0x11));
        assert_eq!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn distinct_keys_for_distinct_wallets() {
        let a = derive_state_key(&sk(0x11));
        let b = derive_state_key(&sk(0x22));
        assert_ne!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn not_the_raw_private_key() {
        // The derived key must not equal the private-key bytes verbatim.
        let pk = sk(0x11);
        let key = derive_state_key(&pk);
        assert_ne!(key.as_bytes(), pk.as_bytes());
    }

    #[test]
    fn derived_key_encrypts_and_decrypts() {
        let key = derive_state_key(&sk(0x33));
        let (ct, nonce) = encrypt(&key, b"chain sync state");
        let again = derive_state_key(&sk(0x33));
        assert_eq!(decrypt(&again, &ct, &nonce).unwrap(), b"chain sync state");
    }
}
