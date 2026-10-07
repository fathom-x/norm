use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::common::Cents;
use crate::generated::SpendAuthorizationStatus;

/// A session spending cap across many orders on one metered listing.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpendAuthorization {
    pub id: String,
    pub listing_id: String,
    pub cap_cents: i64,
    pub spent_cents: Cents,
    pub remaining_cents: Cents,
    pub status: SpendAuthorizationStatus,
    pub expires_at: String,
    pub closed_at: Option<String>,
    pub orders_count: i64,
    pub created_at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

crate::response::data_envelope!(SpendAuthorization);
