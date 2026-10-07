use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

/// An amount in US cents, exactly as the API sent it: an integer for whole
/// cents, a fraction for sub-cent prices (e.g. `0.25`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cents(pub Number);

impl Cents {
    #[must_use]
    pub fn as_f64(&self) -> f64 {
        self.0.as_f64().unwrap_or(0.0)
    }

    /// Whole cents, rounding a fraction up — what to authorize to cover it.
    #[must_use]
    pub fn ceil(&self) -> i64 {
        self.0
            .as_i64()
            .unwrap_or_else(|| self.as_f64().ceil() as i64)
    }

    /// Whole cents, rounded to nearest.
    #[must_use]
    pub fn round(&self) -> i64 {
        self.0
            .as_i64()
            .unwrap_or_else(|| self.as_f64().round() as i64)
    }
}

impl From<i64> for Cents {
    fn from(cents: i64) -> Self {
        Self(cents.into())
    }
}

/// A decimal number the API renders as a string (e.g. `"500.0"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DecimalString(pub String);

impl DecimalString {
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        self.0.trim().parse().ok()
    }
}

/// One page of a list: `{"data": [...], "next_cursor": "..."|null}`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page<T> {
    pub data: Vec<T>,
    /// Pass back as `cursor` for the next page; `None` on the last page.
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl<T: serde::de::DeserializeOwned> crate::response::FromBody for Page<T> {
    fn from_body(body: &Value) -> Result<Self, serde_json::Error> {
        Self::deserialize(body)
    }
}
