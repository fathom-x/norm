//! Cross-language NIP-98 compatibility test (Rust counterpart of
//! `owallet/wallet_mcp/tests/test_nip98_compat.py` and
//! `test/services/nip98_cross_language_test.rb`).
//!
//! Reads `test/fixtures/nip98_vectors.json` — the same file the Python and
//! Ruby suites consume — and asserts that signing the vector inputs through
//! `owallet_crypto::nip98::sign_at` produces a NIP-01 event whose `id`,
//! `pubkey`, `created_at`, `kind`, and `tags` match the captured vector,
//! and whose signature verifies under the wallet x-only pubkey.
//!
//! The vector file deliberately does **not** capture `sig` because BIP-340
//! signatures are non-deterministic (random `aux_rand`); we pass a fixed
//! zero aux here for determinism within this test run, and verify the
//! signature is structurally valid rather than byte-equal across languages.

use std::path::PathBuf;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use owallet_crypto::hd::PrivateKey;
use owallet_crypto::nip98;
use secp256k1::{schnorr, Message, Secp256k1, XOnlyPublicKey};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct VectorFile {
    vectors: Vec<Vector>,
}

#[derive(Debug, Deserialize)]
struct Vector {
    name: String,
    secret_key_hex: String,
    pubkey_hex: String,
    url: String,
    method: String,
    created_at: i64,
    serialized: String,
    event_id: String,
}

fn vectors_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is .../owallet-rs/crates/owallet-crypto, so the
    // repo root is three levels up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../test/fixtures/nip98_vectors.json")
}

fn load_vectors() -> Vec<Vector> {
    let path = vectors_path();
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    let parsed: VectorFile = serde_json::from_str(&raw).expect("vectors.json parses");
    assert!(!parsed.vectors.is_empty(), "vector file is empty");
    parsed.vectors
}

#[test]
fn nip98_vectors_round_trip_in_rust() {
    for v in load_vectors() {
        let sk = PrivateKey::from_hex(&v.secret_key_hex)
            .unwrap_or_else(|e| panic!("vector {} secret_key_hex: {e:?}", v.name));

        let header = nip98::sign_at(&sk, &v.url, &v.method, v.created_at, [0u8; 32]);
        let b64 = header
            .strip_prefix("Nostr ")
            .unwrap_or_else(|| panic!("vector {}: missing 'Nostr ' prefix in {header}", v.name));
        let json_bytes = BASE64
            .decode(b64)
            .unwrap_or_else(|e| panic!("vector {} base64 decode: {e}", v.name));
        let event: Value = serde_json::from_slice(&json_bytes)
            .unwrap_or_else(|e| panic!("vector {} event JSON: {e}", v.name));

        assert_eq!(
            event["id"], v.event_id,
            "vector {}: event id drifted (serialized form must match Python/Ruby)",
            v.name,
        );
        assert_eq!(event["pubkey"], v.pubkey_hex, "vector {}: pubkey", v.name);
        assert_eq!(
            event["created_at"], v.created_at,
            "vector {}: created_at",
            v.name
        );
        assert_eq!(event["kind"], 27235, "vector {}: kind", v.name);

        let tags = event["tags"].as_array().expect("tags is array");
        assert_eq!(
            tags[0],
            Value::Array(vec![Value::from("u"), Value::from(v.url.clone())]),
            "vector {}: u tag",
            v.name,
        );
        assert_eq!(
            tags[1],
            Value::Array(vec![
                Value::from("method"),
                Value::from(v.method.to_uppercase()),
            ]),
            "vector {}: method tag",
            v.name,
        );

        // Sanity: serialized field in the vector reconstructs to the same id.
        let recomputed: [u8; 32] = sha256(v.serialized.as_bytes());
        assert_eq!(
            hex::encode(recomputed),
            v.event_id,
            "vector {}: serialized payload does not hash to event_id",
            v.name,
        );

        // The signature this run produced must verify under the wallet pubkey.
        let id_bytes = hex::decode(event["id"].as_str().unwrap()).unwrap();
        let sig_bytes = hex::decode(event["sig"].as_str().unwrap()).unwrap();
        let pubkey_bytes = hex::decode(event["pubkey"].as_str().unwrap()).unwrap();

        let secp = Secp256k1::verification_only();
        let id_arr: [u8; 32] = id_bytes
            .as_slice()
            .try_into()
            .expect("event id is 32 bytes");
        let msg = Message::from_digest(id_arr);
        let sig = schnorr::Signature::from_slice(&sig_bytes).expect("sig parses");
        let pk = XOnlyPublicKey::from_slice(&pubkey_bytes).expect("pubkey parses");
        secp.verify_schnorr(&sig, &msg, &pk)
            .unwrap_or_else(|e| panic!("vector {}: signature does not verify: {e}", v.name));
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}
