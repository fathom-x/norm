//! HTTP error → response conversion.

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Db(#[from] owallet_db::DbError),
    #[error("template render: {0}")]
    Template(#[from] askama::Error),
    #[error("overpay: {0}")]
    Overpay(#[from] owallet_overpay::OverpayError),
    #[error("bad input: {0}")]
    BadInput(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, body) = match &self {
            AppError::BadInput(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::Db(owallet_db::DbError::Locked) => {
                (StatusCode::SERVICE_UNAVAILABLE, self.to_string())
            }
            _ => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
        };
        // Plain text body — internal errors aren't surfaced to end users
        // beyond the status line and a short message.
        (status, Html(format!("<pre>{body}</pre>"))).into_response()
    }
}
