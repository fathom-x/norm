//! Errors from the Overpay HTTP client.

/// What an Overpay call fails with: the SDK's error. An API error keeps
/// its status, machine-readable code and details
/// ([`overpay_sdk::ApiError`]); it prints as `HTTP <status>: <body>`.
pub type OverpayError = overpay_sdk::Error;
