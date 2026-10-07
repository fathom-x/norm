//! NIP-98 — HTTP authentication via signed Nostr events.
//!
//! A thin wrapper over the standalone `overpay-nip98` crate (shared with
//! the Overpay SDK) that takes the wallet's [`PrivateKey`].
//!
//! Wire format: a kind-27235 Nostr event (with `u` and `method` tags),
//! signed BIP-340 schnorr against the canonical event JSON, the whole event
//! serialised as JSON, then base64-encoded into an `Authorization: Nostr <b64>`
//! header.
//!
//! [`sign_with_body`] adds NIP-98's `payload` tag — the hex SHA-256 of the
//! exact request body — so the signature covers what is being bought, not
//! just the URL: Overpay can check it against the body it received and keep
//! the event as the buyer's authorization of the spend.

use crate::hd::PrivateKey;

pub use overpay_nip98::{InvalidKey, Nip98Signer, SignedEvent};

/// Build a NIP-98 event for `(method, url)` and produce the
/// `Authorization: Nostr <b64>` header value. Fails if `sk` is not a valid
/// secp256k1 secret key (zero, or not below the curve order).
pub fn sign(sk: &PrivateKey, url: &str, method: &str) -> Result<String, InvalidKey> {
    overpay_nip98::sign(sk.as_bytes(), url, method)
}

/// [`sign`] covering a request body too: adds `["payload", sha256(body)]`.
/// The body must be the exact bytes sent — the server hashes what it
/// receives.
pub fn sign_with_body(
    sk: &PrivateKey,
    url: &str,
    method: &str,
    body: &[u8],
) -> Result<String, InvalidKey> {
    overpay_nip98::sign_with_payload(sk.as_bytes(), url, method, body)
}

/// Deterministic [`sign_with_body`], for tests.
pub fn sign_with_body_at(
    sk: &PrivateKey,
    url: &str,
    method: &str,
    body: &[u8],
    created_at: i64,
    aux_rand: [u8; 32],
) -> Result<String, InvalidKey> {
    overpay_nip98::sign_with_payload_at(sk.as_bytes(), url, method, body, created_at, aux_rand)
}

/// Variant of [`sign`] that takes an explicit timestamp + aux randomness so
/// tests can produce deterministic output.
pub fn sign_at(
    sk: &PrivateKey,
    url: &str,
    method: &str,
    created_at: i64,
    aux_rand: [u8; 32],
) -> Result<String, InvalidKey> {
    overpay_nip98::sign_at(sk.as_bytes(), url, method, created_at, aux_rand)
}

/// Lets the Overpay SDK sign requests with a wallet key without seeing
/// its bytes.
impl Nip98Signer for PrivateKey {
    fn sign_nip98_payload(
        &self,
        url: &str,
        method: &str,
        payload: Option<&[u8]>,
    ) -> Result<String, InvalidKey> {
        match payload {
            Some(body) => sign_with_body(self, url, method, body),
            None => sign(self, url, method),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine;
    use secp256k1::{Message, Secp256k1};
    use sha2::{Digest, Sha256};

    /// The NIP-01 event id: sha256 of `[0, pubkey, created_at, kind, tags,
    /// content]`, hex — recomputed here to check what the signer produced.
    fn compute_id(
        pubkey: &str,
        created_at: i64,
        kind: u32,
        tags: &[Vec<String>],
        content: &str,
    ) -> String {
        let canonical = serde_json::to_string(&serde_json::json!([
            0, pubkey, created_at, kind, tags, content
        ]))
        .unwrap();
        hex::encode(Sha256::digest(canonical.as_bytes()))
    }

    const NIP98_KIND: u32 = overpay_nip98::KIND;
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
        let header = sign(&sk, "https://example.com/api", "GET").unwrap();
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
        let header = sign(&sk, "https://example.com", "post").unwrap();
        let b64 = &header[6..];
        let json_bytes = BASE64.decode(b64).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&json_bytes).unwrap();
        assert_eq!(v["tags"][1][1], "POST");
    }

    fn decode(header: &str) -> serde_json::Value {
        serde_json::from_slice(&BASE64.decode(&header[6..]).unwrap()).unwrap()
    }

    /// `sign_with_body` adds the NIP-98 payload tag — the sha256 of the
    /// exact body — and the event id covers it.
    #[test]
    fn body_signature_carries_payload_tag() {
        let sk = fixture_sk();
        let body = br#"{"listing_id":"abc","buyer_note":"hi"}"#;
        let v = decode(
            &sign_with_body_at(
                &sk,
                "https://example.com/api/v1/orders",
                "POST",
                body,
                1_700_000_000,
                [0x22u8; 32],
            )
            .unwrap(),
        );
        assert_eq!(v["tags"][2][0], "payload");
        assert_eq!(v["tags"][2][1], hex::encode(Sha256::digest(body)));

        let tags: Vec<Vec<String>> = serde_json::from_value(v["tags"].clone()).unwrap();
        let id = compute_id(
            v["pubkey"].as_str().unwrap(),
            1_700_000_000,
            NIP98_KIND,
            &tags,
            "",
        );
        assert_eq!(v["id"], id);
    }

    /// Without a body the event is exactly the old two-tag form.
    #[test]
    fn signature_without_body_has_no_payload_tag() {
        let sk = fixture_sk();
        let v = decode(&sign(&sk, "https://example.com", "GET").unwrap());
        assert_eq!(v["tags"].as_array().unwrap().len(), 2);
    }

    /// Two signs of the same `(url, method, ts, aux)` produce identical
    /// signatures (schnorr with fixed aux is deterministic).
    #[test]
    fn deterministic_with_fixed_aux() {
        let sk = fixture_sk();
        let aux = [0xaau8; 32];
        let a = sign_at(&sk, "https://example.com", "GET", 1_700_000_000, aux).unwrap();
        let b = sign_at(&sk, "https://example.com", "GET", 1_700_000_000, aux).unwrap();
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
        )
        .unwrap();
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

    /// `PrivateKey` only checks its length, so a zero key reaches the
    /// signer: it must come back as an error, not a panic.
    #[test]
    fn an_invalid_key_is_an_error() {
        let zero = PrivateKey([0u8; 32]);
        assert_eq!(sign(&zero, "https://example.com", "GET"), Err(InvalidKey));
        assert_eq!(
            zero.sign_nip98_payload("https://example.com", "POST", Some(b"{}")),
            Err(InvalidKey)
        );
    }
}
