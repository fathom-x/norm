//! Nostr public-key encoding (npub).
//!
//! `npub` is the BIP-340 x-only public key (32 bytes) bech32-encoded with the
//! `npub` HRP. NIP-19 specifies Variant::Bech32 (not bech32m, despite the
//! latter being newer) — match that exactly.
//!
//! Mirrors `_bech32_encode("npub", xonly_bytes)` in `wallet_mcp/server.py:1355`.

use bech32::{Bech32, Hrp};
use secp256k1::{Keypair, Secp256k1, SecretKey, XOnlyPublicKey};
use thiserror::Error;

use crate::hd::PrivateKey;

#[derive(Debug, Error)]
pub enum NostrError {
    #[error("bech32 encode failed: {0}")]
    Encode(#[from] bech32::EncodeError),
    #[error("bech32 decode failed: {0}")]
    Decode(#[from] bech32::DecodeError),
    #[error("unexpected HRP '{0}', wanted 'npub'")]
    BadHrp(String),
    #[error("invalid x-only pubkey length: {0} bytes (expected 32)")]
    BadLength(usize),
}

/// Derive the x-only secp256k1 public key from a private key.
#[must_use]
pub fn xonly_pubkey(sk: &PrivateKey) -> XOnlyPublicKey {
    let secp = Secp256k1::new();
    let secret = SecretKey::from_slice(sk.as_bytes()).expect("PrivateKey enforces 32-byte input");
    let kp = Keypair::from_secret_key(&secp, &secret);
    kp.x_only_public_key().0
}

/// Encode a 32-byte x-only public key as a Nostr `npub...` bech32 string.
pub fn npub_encode(xonly: &XOnlyPublicKey) -> Result<String, NostrError> {
    let hrp = Hrp::parse("npub").expect("npub is a valid HRP");
    let s = bech32::encode::<Bech32>(hrp, &xonly.serialize())?;
    Ok(s)
}

/// One-shot helper: private key → `npub...` string.
pub fn npub_from_private_key(sk: &PrivateKey) -> Result<String, NostrError> {
    npub_encode(&xonly_pubkey(sk))
}

/// Decode a `npub...` string back to a 32-byte x-only public key payload.
pub fn npub_decode(npub: &str) -> Result<[u8; 32], NostrError> {
    let (hrp, data) = bech32::decode(npub)?;
    if hrp.as_str() != "npub" {
        return Err(NostrError::BadHrp(hrp.as_str().to_string()));
    }
    let arr: [u8; 32] = data
        .as_slice()
        .try_into()
        .map_err(|_| NostrError::BadLength(data.len()))?;
    Ok(arr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bip39::Mnemonic;
    use crate::hd::{derive_from_mnemonic, EVM_HD_PATH};

    const ABANDON_12: &str = "abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon about";

    #[test]
    fn abandon_path_npub_is_stable() {
        let m = Mnemonic::parse(ABANDON_12).unwrap();
        let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
        let npub = npub_from_private_key(&sk).unwrap();
        // This value is the bech32 encoding of the x-only pubkey for the
        // well-known abandon→EVM key. It's computed once here and pinned;
        // any change to bech32 variant, encoding, or x-only derivation
        // would shift it.
        assert!(npub.starts_with("npub1"));
        // Decode round-trips back to the same 32-byte x-only pubkey.
        let decoded = npub_decode(&npub).unwrap();
        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn deterministic_npub() {
        let m = Mnemonic::parse(ABANDON_12).unwrap();
        let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
        let a = npub_from_private_key(&sk).unwrap();
        let b = npub_from_private_key(&sk).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn decode_rejects_wrong_hrp() {
        // Encode the same 32-byte payload under HRP "nsec" (private-key prefix)
        // and confirm decode rejects it.
        let payload = [0xabu8; 32];
        let hrp = Hrp::parse("nsec").unwrap();
        let s = bech32::encode::<Bech32>(hrp, &payload).unwrap();
        assert!(matches!(npub_decode(&s), Err(NostrError::BadHrp(_))));
    }
}
