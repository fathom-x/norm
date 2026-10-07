//! NIP-98 — HTTP authentication with signed Nostr events.
//!
//! A request is authenticated by an `Authorization: Nostr <base64>` header
//! whose payload is a kind-27235 Nostr event, BIP-340-signed, with tags:
//!
//! - `u` — the exact request URL, query string included (Overpay compares
//!   it byte-for-byte against the URL it received, so sign the final URL);
//! - `method` — the HTTP method, upper-cased;
//! - optionally `payload` — the hex SHA-256 of the request body.
//!
//! Overpay accepts events up to 60 seconds old, so sign each request (and
//! each retry of it) when it is sent.
//!
//! ```
//! use overpay_nip98::{Nip98Signer, SecretKey};
//!
//! let key = SecretKey::from_bytes([7u8; 32]).unwrap();
//! let header = key.sign_nip98("https://overpay.example/api/v1/orders", "GET").unwrap();
//! assert!(header.starts_with("Nostr "));
//! ```

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use secp256k1::{Keypair, Message, SECP256K1};
use serde::Serialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

/// The Nostr event kind NIP-98 uses for HTTP auth.
pub const KIND: u32 = 27235;

/// Anything that can produce a NIP-98 header value for a request.
/// Implemented by [`SecretKey`]; wallets implement it for their own key
/// types so an HTTP client can sign without seeing raw key bytes.
pub trait Nip98Signer: Send + Sync {
    /// The header value (`Nostr <base64 event>`) for `method url`, signed
    /// now, with a `payload` tag binding `payload` when given: the exact
    /// request body bytes, so the signature covers what is sent and not
    /// just where. Fails only if the signer's key is not a valid secp256k1
    /// secret key.
    fn sign_nip98_payload(
        &self,
        url: &str,
        method: &str,
        payload: Option<&[u8]>,
    ) -> Result<String, InvalidKey>;

    /// [`Self::sign_nip98_payload`] without a body.
    fn sign_nip98(&self, url: &str, method: &str) -> Result<String, InvalidKey> {
        self.sign_nip98_payload(url, method, None)
    }
}

/// A secp256k1 secret key, zeroized on drop.
#[derive(Clone)]
pub struct SecretKey([u8; 32]);

impl SecretKey {
    /// Wrap 32 secret-key bytes. Fails if they are not a valid secp256k1
    /// scalar (zero, or not below the curve order).
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, InvalidKey> {
        secp256k1::SecretKey::from_slice(&bytes).map_err(|_| InvalidKey)?;
        Ok(Self(bytes))
    }

    /// Parse a hex-encoded key (an optional `0x` prefix is allowed).
    pub fn from_hex(s: &str) -> Result<Self, InvalidKey> {
        let s = s.trim();
        let s = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s);
        let bytes: [u8; 32] = hex::decode(s)
            .map_err(|_| InvalidKey)?
            .try_into()
            .map_err(|_| InvalidKey)?;
        Self::from_bytes(bytes)
    }

    /// The x-only public key, hex — the event's `pubkey`.
    #[must_use]
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.keypair().x_only_public_key().0.serialize())
    }

    fn keypair(&self) -> Keypair {
        Keypair::from_seckey_slice(SECP256K1, &self.0).expect("validated by from_bytes")
    }
}

impl Drop for SecretKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(..)")
    }
}

impl Nip98Signer for SecretKey {
    fn sign_nip98_payload(
        &self,
        url: &str,
        method: &str,
        payload: Option<&[u8]>,
    ) -> Result<String, InvalidKey> {
        Ok(header(&signed_event(
            &self.keypair(),
            url,
            method,
            payload,
            now_secs(),
            rand_aux(),
        )))
    }
}

/// The bytes given are not a valid secp256k1 secret key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidKey;

impl fmt::Display for InvalidKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not a valid secp256k1 secret key")
    }
}

impl std::error::Error for InvalidKey {}

/// A signed Nostr event, as carried in the header.
#[derive(Debug, Clone, Serialize)]
pub struct SignedEvent {
    pub id: String,
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

/// The `Authorization` header value for `method url`, signed now. Fails if
/// `sk` is not a valid secp256k1 secret key (zero, or not below the curve
/// order).
pub fn sign(sk: &[u8; 32], url: &str, method: &str) -> Result<String, InvalidKey> {
    sign_at(sk, url, method, now_secs(), rand_aux())
}

/// [`sign`] with a `payload` tag binding the request body.
pub fn sign_with_payload(
    sk: &[u8; 32],
    url: &str,
    method: &str,
    body: &[u8],
) -> Result<String, InvalidKey> {
    Ok(header(&event(
        sk,
        url,
        method,
        Some(body),
        now_secs(),
        rand_aux(),
    )?))
}

/// [`sign_with_payload`] at a fixed time and BIP-340 auxiliary randomness,
/// for deterministic output in tests.
pub fn sign_with_payload_at(
    sk: &[u8; 32],
    url: &str,
    method: &str,
    body: &[u8],
    created_at: i64,
    aux_rand: [u8; 32],
) -> Result<String, InvalidKey> {
    Ok(header(&event(
        sk,
        url,
        method,
        Some(body),
        created_at,
        aux_rand,
    )?))
}

/// [`sign`] at a fixed time and BIP-340 auxiliary randomness, for
/// deterministic output in tests.
pub fn sign_at(
    sk: &[u8; 32],
    url: &str,
    method: &str,
    created_at: i64,
    aux_rand: [u8; 32],
) -> Result<String, InvalidKey> {
    Ok(header(&event(sk, url, method, None, created_at, aux_rand)?))
}

/// Build and sign the event without encoding it. Fails if `sk` is not a
/// valid secp256k1 secret key.
pub fn event(
    sk: &[u8; 32],
    url: &str,
    method: &str,
    payload: Option<&[u8]>,
    created_at: i64,
    aux_rand: [u8; 32],
) -> Result<SignedEvent, InvalidKey> {
    let keypair = Keypair::from_seckey_slice(SECP256K1, sk).map_err(|_| InvalidKey)?;
    Ok(signed_event(
        &keypair, url, method, payload, created_at, aux_rand,
    ))
}

fn signed_event(
    keypair: &Keypair,
    url: &str,
    method: &str,
    payload: Option<&[u8]>,
    created_at: i64,
    aux_rand: [u8; 32],
) -> SignedEvent {
    let pubkey = hex::encode(keypair.x_only_public_key().0.serialize());

    let mut tags = vec![
        vec!["u".to_string(), url.to_string()],
        vec!["method".to_string(), method.to_uppercase()],
    ];
    if let Some(body) = payload {
        tags.push(vec![
            "payload".to_string(),
            hex::encode(Sha256::digest(body)),
        ]);
    }
    let content = String::new();

    let id = event_id(&pubkey, created_at, KIND, &tags, &content);
    let msg = Message::from_digest(id);
    let sig = SECP256K1.sign_schnorr_with_aux_rand(&msg, keypair, &aux_rand);

    SignedEvent {
        id: hex::encode(id),
        pubkey,
        created_at,
        kind: KIND,
        tags,
        content,
        sig: hex::encode(sig.as_ref()),
    }
}

/// `Nostr <base64 of the event JSON>`.
#[must_use]
pub fn header(event: &SignedEvent) -> String {
    let json = serde_json::to_string(event).expect("event serializes");
    format!("Nostr {}", BASE64.encode(json.as_bytes()))
}

/// The x-only public key of `sk`, hex. Fails if `sk` is not a valid
/// secp256k1 secret key.
pub fn public_key_hex(sk: &[u8; 32]) -> Result<String, InvalidKey> {
    Ok(SecretKey::from_bytes(*sk)?.public_key_hex())
}

/// NIP-01 event id: SHA-256 of `[0, pubkey, created_at, kind, tags, content]`
/// serialized without whitespace.
fn event_id(
    pubkey: &str,
    created_at: i64,
    kind: u32,
    tags: &[Vec<String>],
    content: &str,
) -> [u8; 32] {
    let canonical = serde_json::to_string(&serde_json::json!([
        0, pubkey, created_at, kind, tags, content
    ]))
    .expect("value serializes");
    Sha256::digest(canonical.as_bytes()).into()
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

    const KEY: [u8; 32] = [0x11; 32];

    fn decode(header: &str) -> serde_json::Value {
        let b64 = header.strip_prefix("Nostr ").expect("Nostr prefix");
        serde_json::from_slice(&BASE64.decode(b64).unwrap()).unwrap()
    }

    #[test]
    fn header_carries_url_and_upper_cased_method() {
        let v = decode(&sign(&KEY, "https://example.com/api?x=1", "post").unwrap());
        assert_eq!(v["kind"], KIND);
        assert_eq!(
            v["tags"],
            serde_json::json!([["u", "https://example.com/api?x=1"], ["method", "POST"]])
        );
        assert_eq!(v["content"], "");
        assert_eq!(v["pubkey"], public_key_hex(&KEY).unwrap());
    }

    #[test]
    fn fixed_time_and_aux_are_deterministic() {
        let a = sign_at(
            &KEY,
            "https://example.com",
            "GET",
            1_700_000_000,
            [0xaa; 32],
        )
        .unwrap();
        let b = sign_at(
            &KEY,
            "https://example.com",
            "GET",
            1_700_000_000,
            [0xaa; 32],
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn signature_verifies_against_the_id() {
        let v = decode(
            &sign_at(
                &KEY,
                "https://example.com",
                "GET",
                1_700_000_000,
                [0x22; 32],
            )
            .unwrap(),
        );
        let id: [u8; 32] = hex::decode(v["id"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let sig = secp256k1::schnorr::Signature::from_slice(
            &hex::decode(v["sig"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        let pk = secp256k1::XOnlyPublicKey::from_slice(
            &hex::decode(v["pubkey"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        SECP256K1
            .verify_schnorr(&sig, &Message::from_digest(id), &pk)
            .expect("valid signature");
    }

    #[test]
    fn payload_tag_is_the_body_hash() {
        let v = decode(&sign_with_payload(&KEY, "https://example.com", "POST", b"{}").unwrap());
        assert_eq!(
            v["tags"][2],
            serde_json::json!([
                "payload",
                "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
            ])
        );
    }

    #[test]
    fn secret_key_validates_and_signs() {
        assert!(SecretKey::from_bytes([0; 32]).is_err());
        assert!(SecretKey::from_hex("zz").is_err());
        let key = SecretKey::from_hex(&format!("0x{}", hex::encode(KEY))).unwrap();
        assert_eq!(key.public_key_hex(), public_key_hex(&KEY).unwrap());
        assert_eq!(
            decode(&key.sign_nip98("https://e.com", "get").unwrap())["tags"][1][1],
            "GET"
        );
        assert_eq!(format!("{key:?}"), "SecretKey(..)");
    }

    #[test]
    fn invalid_raw_keys_are_errors_not_panics() {
        // Zero, and the curve order n itself: neither is a secret key.
        let order: [u8; 32] =
            hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141")
                .unwrap()
                .try_into()
                .unwrap();
        for bad in [[0u8; 32], order, [0xff; 32]] {
            assert_eq!(sign(&bad, "https://e.com", "GET"), Err(InvalidKey));
            assert_eq!(
                sign_with_payload(&bad, "https://e.com", "POST", b"{}"),
                Err(InvalidKey)
            );
            assert!(event(&bad, "https://e.com", "GET", None, 0, [0; 32]).is_err());
            assert_eq!(public_key_hex(&bad), Err(InvalidKey));
        }
    }
}
