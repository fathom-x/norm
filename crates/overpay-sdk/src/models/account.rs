use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The authenticated buyer.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub account_number: String,
    pub formatted_account_number: String,
    pub username: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `POST /api/v1/buyer/register` options.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BuyerRegistrationRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_name: Option<String>,
}

/// A buyer account found or created for a Nostr key, with a new API token.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuyerRegistration {
    pub user_id: String,
    pub account_number: String,
    pub nostr_pubkey: String,
    /// The new API token. Shown once.
    pub token: String,
    pub token_name: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A one-time browser login link (5-minute TTL).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebSession {
    pub login_url: String,
    /// Unix time.
    pub expires_at: i64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

crate::response::data_envelope!(Account, BuyerRegistration, WebSession);
