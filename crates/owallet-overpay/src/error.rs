//! Error types for the Overpay HTTP client.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum OverpayError {
    #[error("http transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },
    #[error("response is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("url parse: {0}")]
    Url(#[from] url::ParseError),
    #[error("nip98 sign requires a private key for unauthenticated requests")]
    AuthRequired,
    #[error("nip98 sign: {0}")]
    Sign(String),
}
