//! NIP-98 — HTTP authentication via signed Nostr events.
//!
//! Ports `_sign_nip98(url, method)` from `wallet_mcp/server.py:1383-1409`.
//!
//! Wire format: a kind-27235 Nostr event (with `u` and `method` tags),
//! signed BIP-340 schnorr against the canonical event JSON, the whole event
//! serialised as JSON, then base64-encoded into an `Authorization: Nostr <b64>`
//! header.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use secp256k1::{Keypair, Message, Secp256k1, SecretKey};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::hd::PrivateKey;

const NIP98_KIND: u32 = 27235;

/// JSON shape of a signed Nostr event, including `id` and `sig`.
#[derive(Debug, Serialize)]
pub struct SignedEvent {
    pub id: String,
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

/// Build a NIP-98 event for `(method, url)` and produce the
/// `Authorization: Nostr <b64>` header value.
pub fn sign(sk: &PrivateKey, url: &str, method: &str) -> String {
    sign_at(sk, url, method, now_secs(), rand_aux())
}

/// Variant of [`sign`] that takes an explicit timestamp + aux randomness so
/// tests can produce deterministic output.
pub fn sign_at(
    sk: &PrivateKey,
    url: &str,
    method: &str,
    created_at: i64,
    aux_rand: [u8; 32],
) -> String {
    let secp = Secp256k1::new();
    let secret = SecretKey::from_slice(sk.as_bytes()).expect("PrivateKey enforces 32-byte input");
    let kp = Keypair::from_secret_key(&secp, &secret);
    let xonly = kp.x_only_public_key().0.serialize();
    let pubkey_hex = hex::encode(xonly);

    let tags = vec![
        vec!["u".to_string(), url.to_string()],
        vec!["method".to_string(), method.to_uppercase()],
    ];
    let content = String::new();

    let id = compute_id(&pubkey_hex, created_at, NIP98_KIND, &tags, &content);
    let id_bytes = hex::decode(&id).expect("compute_id returns valid hex");
    let id_arr: [u8; 32] = id_bytes
        .as_slice()
        .try_into()
        .expect("sha256 always produces 32 bytes");
    let msg = Message::from_digest(id_arr);
    let sig = secp.sign_schnorr_with_aux_rand(&msg, &kp, &aux_rand);

    let event = SignedEvent {
        id,
        pubkey: pubkey_hex,
        created_at,
        kind: NIP98_KIND,
        tags,
        content,
        sig: hex::encode(sig.as_ref()),
    };

    let json = serde_json::to_string(&event).expect("event serialises");
    format!("Nostr {}", BASE64.encode(json.as_bytes()))
}

/// Compute the canonical NIP-01 event id: SHA-256 of the JSON-serialised
/// array `[0, pubkey, created_at, kind, tags, content]` with no extra
/// whitespace.
fn compute_id(
    pubkey: &str,
    created_at: i64,
    kind: u32,
    tags: &[Vec<String>],
    content: &str,
) -> String {
    // Build the canonical array via serde_json::Value to guarantee the
    // separator-less, no-extra-whitespace output that NIP-01 mandates.
    let value = serde_json::json!([0, pubkey, created_at, kind, tags, content]);
    let canon = serde_json::to_string(&value).expect("Value serialises");
    let mut hasher = Sha256::new();
    hasher.update(canon.as_bytes());
    hex::encode(hasher.finalize())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn rand_aux() -> [u8; 32] {
    let mut aux = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut aux);
    aux
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bip39::Mnemonic;
    use crate::hd::{derive_from_mnemonic, EVM_HD_PATH};

    const ABANDON_12: &str = "abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon about";

    fn fixture_sk() -> PrivateKey {
        let m = Mnemonic::parse(ABANDON_12).unwrap();
        derive_from_mnemonic(&m, EVM_HD_PATH).unwrap()
    }

    #[test]
    fn header_has_nostr_prefix_and_base64_body() {
        let sk = fixture_sk();
        let header = sign(&sk, "https://example.com/api", "GET");
        assert!(header.starts_with("Nostr "));
        let b64 = &header[6..];
        // The base64 body should decode to JSON containing the expected fields.
        let json_bytes = BASE64.decode(b64).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&json_bytes).unwrap();
        assert_eq!(v["kind"], NIP98_KIND);
        assert_eq!(v["tags"][0][0], "u");
        assert_eq!(v["tags"][0][1], "https://example.com/api");
        assert_eq!(v["tags"][1][0], "method");
        assert_eq!(v["tags"][1][1], "GET");
        assert!(v["sig"].as_str().unwrap().len() == 128); // 64 bytes hex
        assert!(v["id"].as_str().unwrap().len() == 64); // 32 bytes hex
        assert!(v["pubkey"].as_str().unwrap().len() == 64);
    }

    #[test]
    fn method_is_uppercased() {
        let sk = fixture_sk();
        let header = sign(&sk, "https://example.com", "post");
        let b64 = &header[6..];
        let json_bytes = BASE64.decode(b64).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&json_bytes).unwrap();
        assert_eq!(v["tags"][1][1], "POST");
    }

    /// Two signs of the same `(url, method, ts, aux)` produce identical
    /// signatures (schnorr with fixed aux is deterministic).
    #[test]
    fn deterministic_with_fixed_aux() {
        let sk = fixture_sk();
        let aux = [0xaau8; 32];
        let a = sign_at(&sk, "https://example.com", "GET", 1_700_000_000, aux);
        let b = sign_at(&sk, "https://example.com", "GET", 1_700_000_000, aux);
        assert_eq!(a, b);
    }

    /// The signature verifies against the event id under the wallet's
    /// x-only public key.
    #[test]
    fn signature_verifies() {
        let sk = fixture_sk();
        let header = sign_at(
            &sk,
            "https://example.com",
            "GET",
            1_700_000_000,
            [0x11u8; 32],
        );
        let b64 = &header[6..];
        let json_bytes = BASE64.decode(b64).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&json_bytes).unwrap();

        let id = hex::decode(v["id"].as_str().unwrap()).unwrap();
        let sig_bytes = hex::decode(v["sig"].as_str().unwrap()).unwrap();
        let pubkey_bytes = hex::decode(v["pubkey"].as_str().unwrap()).unwrap();

        let secp = Secp256k1::verification_only();
        let id_arr: [u8; 32] = id.as_slice().try_into().unwrap();
        let msg = Message::from_digest(id_arr);
        let sig = secp256k1::schnorr::Signature::from_slice(&sig_bytes).unwrap();
        let pk = secp256k1::XOnlyPublicKey::from_slice(&pubkey_bytes).unwrap();
        secp.verify_schnorr(&sig, &msg, &pk).expect("valid sig");
    }
}
