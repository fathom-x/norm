//! The API's operations, as listed in the spec (see [`crate::OPERATIONS`]).

/// Who an operation is for.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// Buyers and their wallets — what this SDK implements.
    Buyer,
    /// Seller bots (the Seller API).
    Seller,
    /// The x402 gateway's server-to-server callback.
    Gateway,
    /// The API index and its documentation.
    Meta,
}

/// One operation of the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operation {
    /// The spec's `operationId`.
    pub id: &'static str,
    /// HTTP method, upper-case.
    pub method: &'static str,
    /// Path template, e.g. `/api/v1/orders/{id}`.
    pub path: &'static str,
    pub audience: Audience,
}
