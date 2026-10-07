//! How a request authenticates.

pub use overpay_nip98::{InvalidKey, Nip98Signer, SecretKey};

use crate::error::Error;

/// The header carrying a NIP-98 signature next to a bearer token
/// ([`Auth::BearerSigned`]).
pub const SIGNATURE_HEADER: &str = "x-nostr-signature";

/// The credentials for one request.
///
/// Signatures ([`Auth::Nip98`], [`Auth::BearerSigned`]) are made per request
/// and per retry over the final URL and method, and for a request with a
/// body they carry NIP-98's `payload` tag: the sha256 of the exact bytes
/// sent. The client serialises a body once and sends those same bytes.
#[non_exhaustive]
#[derive(Clone, Copy)]
pub enum Auth<'a> {
    /// No `Authorization` header (public endpoints: listings, sellers).
    None,
    /// An API token: `Authorization: Bearer <token>`.
    Bearer(&'a str),
    /// A NIP-98 signature by a Nostr key in `Authorization: Nostr <event>`.
    Nip98(&'a dyn Nip98Signer),
    /// An API token, plus a NIP-98 signature by the buyer's key in
    /// `X-Nostr-Signature` — the keyholder's signed authorization of each
    /// request (for a spend, of exactly what is bought).
    BearerSigned(&'a str, &'a dyn Nip98Signer),
}

impl std::fmt::Debug for Auth<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::None => "Auth::None",
            Self::Bearer(_) => "Auth::Bearer(..)",
            Self::Nip98(_) => "Auth::Nip98(..)",
            Self::BearerSigned(..) => "Auth::BearerSigned(..)",
        })
    }
}

/// The auth headers for one attempt of a request.
pub(crate) struct AuthHeaders {
    pub authorization: Option<String>,
    pub signature: Option<String>,
}

impl Auth<'_> {
    /// The headers for `method url` with `body` (the exact bytes sent).
    /// Fails ([`Error::Sign`]) only if a signer's key is invalid.
    pub(crate) fn headers(
        &self,
        method: &str,
        url: &str,
        body: Option<&[u8]>,
    ) -> Result<AuthHeaders, Error> {
        let sign = |signer: &dyn Nip98Signer| {
            signer
                .sign_nip98_payload(url, method, body)
                .map_err(|e| Error::Sign(e.to_string()))
        };
        Ok(match self {
            Self::None => AuthHeaders {
                authorization: None,
                signature: None,
            },
            Self::Bearer(token) => AuthHeaders {
                authorization: Some(format!("Bearer {token}")),
                signature: None,
            },
            Self::Nip98(signer) => AuthHeaders {
                authorization: Some(sign(*signer)?),
                signature: None,
            },
            Self::BearerSigned(token, signer) => AuthHeaders {
                authorization: Some(format!("Bearer {token}")),
                signature: Some(sign(*signer)?),
            },
        })
    }
}
