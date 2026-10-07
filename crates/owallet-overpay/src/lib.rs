//! owallet's Overpay API client: a compatibility layer over
//! [`overpay_sdk`], which does the HTTP. Endpoints:
//! - `POST /oauth/token` (PKCE code exchange)
//! - `GET /api/v1/account`
//! - `GET /api/v1/orders` and `/orders/:id`
//! - `POST /api/v1/orders` (create)
//! - `GET /api/v1/listings` and `/listings/:id`
//! - `GET|POST /api/v1/merchant_credits[/:seller_slug][/purchase|/redeem]`
//! - `POST /api/v1/buyer/web_session`
//!
//! Auth modes ([`overpay_sdk::Auth`]; a `&PrivateKey` is a `Nip98Signer`):
//! - `Auth::None` — unauthenticated GETs (e.g. marketplace listings)
//! - `Auth::Bearer(token)` — stored OAuth access token
//! - `Auth::Nip98(sk)` — wallet-based fallback, signed per request by
//!   the SDK through `owallet_crypto`'s `Nip98Signer` impl
//! - `Auth::BearerSigned(token, sk)` — the token, plus the wallet key's
//!   NIP-98 signature in `X-Nostr-Signature` (POST bodies covered by the
//!   `payload` tag), which Overpay stores against each spend

pub mod client;
pub mod error;
pub mod models;

pub use client::{host_key, OverpayClient};
pub use error::OverpayError;
pub use overpay_sdk::Auth;
