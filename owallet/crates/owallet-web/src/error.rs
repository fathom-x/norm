//! `/_mgmt` errors: a non-2xx status with
//! `{"error": {"code": "<snake_case>", "message": "<human readable>"}}`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

#[derive(Debug)]
pub struct MgmtError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl MgmtError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
    }

    pub fn db(e: owallet_db::DbError) -> Self {
        match e {
            owallet_db::DbError::Locked => Self::locked(),
            e => Self::internal(format!("database: {e}")),
        }
    }

    pub fn not_initialized() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "not_initialized",
            "no wallet database yet — POST /_mgmt/init first",
        )
    }

    pub fn locked() -> Self {
        Self::conflict(
            "locked",
            "the wallet database is locked — POST /_mgmt/unlock first",
        )
    }

    pub fn no_wallet() -> Self {
        Self::conflict(
            "no_wallet",
            "no wallet selected — POST /_mgmt/generate or /_mgmt/import first",
        )
    }

    pub fn not_found(path: &str) -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("no route for {path}"),
        )
    }

    pub fn overpay(e: owallet_overpay::OverpayError) -> Self {
        match e {
            owallet_overpay::OverpayError::HttpStatus { status, body } => Self::new(
                StatusCode::BAD_GATEWAY,
                "overpay_error",
                format!("Overpay answered HTTP {status}: {}", truncate(&body, 300)),
            ),
            e => Self::new(
                StatusCode::BAD_GATEWAY,
                "overpay_unreachable",
                format!("could not reach Overpay: {e}"),
            ),
        }
    }
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

impl std::fmt::Display for MgmtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl IntoResponse for MgmtError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({
                "error": {"code": self.code, "message": self.message},
            })),
        )
            .into_response()
    }
}
