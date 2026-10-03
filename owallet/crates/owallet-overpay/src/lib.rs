//! REST client for the Overpay Rails API.
//!
//! Mirrors the HTTP calls in `wallet_mcp/server.py`:
//! - `POST /oauth/clients` (dynamic OAuth client registration)
//! - `POST /oauth/token` (PKCE code exchange)
//! - `GET /api/v1/account`
//! - `GET /api/v1/orders` and `/orders/:id`
//! - `POST /api/v1/orders` (create)
//! - `GET /api/v1/listings` and `/listings/:id`
//! - `GET|POST /api/v1/merchant_credits[/:seller_slug][/purchase|/redeem]`
//! - `POST /api/v1/buyer/web_session`
//! - `POST /api/v1/buyer/register` (NIP-98: wallet-key sign-up)
//!
//! Auth modes:
//! - `Auth::None` — unauthenticated GETs (e.g. marketplace listings)
//! - `Auth::Bearer(token)` — stored OAuth access token
//! - `Auth::Nip98 { sk }` — wallet-based fallback (wired into endpoint
//!   methods directly; the actual NIP-98 signing happens in
//!   `owallet_crypto::nip98`)

pub mod client;
pub mod compat;
pub mod error;
pub mod models;
pub mod pkce;

pub use client::{host_key, Auth, DeliveredContent, OverpayClient, PkceLogin};
pub use compat::sendable;
pub use error::OverpayError;
pub use pkce::Pkce;
