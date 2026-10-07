//! Responses: the raw JSON, with typed access on demand.

use std::marker::PhantomData;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::Error;

/// A 2xx response's status and JSON body.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct RawResponse {
    pub status: u16,
    /// True when the API replayed an earlier response to the same
    /// idempotency key instead of acting again.
    pub replayed: bool,
    pub body: Value,
}

/// A successful response: the JSON exactly as the API sent it, parsed into
/// `T` only when asked ([`Response::parse`]). Code that forwards API output
/// verbatim reads [`Response::raw`] and never pays for (or fails on) the
/// typed view.
#[derive(Debug, Clone)]
pub struct Response<T> {
    raw: RawResponse,
    _type: PhantomData<fn() -> T>,
}

impl<T> Response<T> {
    pub(crate) fn new(raw: RawResponse) -> Self {
        Self {
            raw,
            _type: PhantomData,
        }
    }

    /// The whole body, envelope included (`{"data": ...}`).
    #[must_use]
    pub fn raw(&self) -> &Value {
        &self.raw.body
    }

    #[must_use]
    pub fn into_raw(self) -> Value {
        self.raw.body
    }

    /// The body's `data`, or the whole body when it has none.
    #[must_use]
    pub fn data(&self) -> &Value {
        self.raw.body.get("data").unwrap_or(&self.raw.body)
    }

    #[must_use]
    pub fn status(&self) -> u16 {
        self.raw.status
    }

    /// See [`RawResponse::replayed`].
    #[must_use]
    pub fn replayed(&self) -> bool {
        self.raw.replayed
    }
}

impl<T: FromBody> Response<T> {
    /// The typed view of the body.
    pub fn parse(&self) -> Result<T, Error> {
        Ok(T::from_body(&self.raw.body)?)
    }
}

/// How a type is read out of a response body.
pub trait FromBody: Sized {
    fn from_body(body: &Value) -> Result<Self, serde_json::Error>;
}

/// Types that arrive as `{"data": <T>}`.
pub(crate) fn from_data<T: DeserializeOwned>(body: &Value) -> Result<T, serde_json::Error> {
    T::deserialize(body.get("data").unwrap_or(body))
}

/// Types read from the `data` envelope.
macro_rules! data_envelope {
    ($($t:ty),* $(,)?) => {
        $(impl $crate::response::FromBody for $t {
            fn from_body(body: &serde_json::Value) -> Result<Self, serde_json::Error> {
                $crate::response::from_data(body)
            }
        })*
    };
}

/// Types that are the whole body (no envelope).
macro_rules! whole_body {
    ($($t:ty),* $(,)?) => {
        $(impl $crate::response::FromBody for $t {
            fn from_body(body: &serde_json::Value) -> Result<Self, serde_json::Error> {
                <$t as serde::Deserialize>::deserialize(body)
            }
        })*
    };
}

pub(crate) use {data_envelope, whole_body};

impl<T: DeserializeOwned> FromBody for Vec<T> {
    fn from_body(body: &Value) -> Result<Self, serde_json::Error> {
        from_data(body)
    }
}

impl FromBody for Value {
    fn from_body(body: &Value) -> Result<Self, serde_json::Error> {
        Ok(body.clone())
    }
}
