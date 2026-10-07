//! Downloading an offloaded deliverable (`delivered_content_url`).

use reqwest::header::CONTENT_TYPE;
use url::Url;

use crate::error::{ApiError, Error};
use crate::Client;

/// What [`Client::fetch_delivered_content`] found behind a delivered file's link.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveredContent {
    /// UTF-8 text, safe to inline as `delivered_content`.
    Text(String),
    /// Anything else (an image, audio, a PDF…): its media type and size,
    /// when known. The bytes are not kept.
    Binary {
        content_type: Option<String>,
        bytes: Option<u64>,
    },
}

impl Client {
    /// Largest delivered file [`Self::fetch_delivered_content`] reads.
    pub const MAX_DELIVERED_CONTENT_BYTES: usize = 16 * 1024 * 1024;

    /// Fetch an order's result when the marketplace delivered it as a file
    /// (`delivered_content_url`) instead of inline `delivered_content`. The
    /// link is signed, so no auth header is sent; it must point at this
    /// marketplace's own origin (base or public URL) — the redirect it
    /// answers with, to blob storage, is followed.
    ///
    /// Text comes back as [`DeliveredContent::Text`]. Anything else — an
    /// image, a PDF, bytes that are not UTF-8 — is a successful delivery
    /// too, returned as [`DeliveredContent::Binary`] (type and size) for the
    /// caller to hand on as a link. A file whose `Content-Type` already says
    /// binary is not downloaded at all.
    pub async fn fetch_delivered_content(&self, url: &str) -> Result<DeliveredContent, Error> {
        let target = Url::parse(url)?;
        let same_origin = |known: &Url| {
            known.scheme() == target.scheme()
                && known.host_str() == target.host_str()
                && known.port_or_known_default() == target.port_or_known_default()
        };
        if !same_origin(self.base_url()) && !same_origin(self.public_url()) {
            return Err(Error::Delivery(format!(
                "refusing to fetch from {}, which is not this marketplace",
                target.host_str().unwrap_or("an unknown host")
            )));
        }
        let resp = self.http().get(target).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.bytes().await.unwrap_or_default();
            return Err(ApiError::from_response(status.as_u16(), &body).into());
        }
        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty());
        if content_type.as_deref().is_some_and(is_binary_media_type) {
            return Ok(DeliveredContent::Binary {
                content_type,
                bytes: resp.content_length(),
            });
        }
        let bytes = resp.bytes().await?;
        if bytes.len() > Self::MAX_DELIVERED_CONTENT_BYTES {
            return Err(Error::Delivery(format!(
                "file is {} bytes, over the {}-byte limit",
                bytes.len(),
                Self::MAX_DELIVERED_CONTENT_BYTES
            )));
        }
        let len = bytes.len() as u64;
        Ok(match String::from_utf8(bytes.to_vec()) {
            Ok(text) => DeliveredContent::Text(text),
            Err(_) => DeliveredContent::Binary {
                content_type,
                bytes: Some(len),
            },
        })
    }
}

/// Media types that are never text, so there is no point downloading them
/// to find out. Unknown and `application/octet-stream` files are still read
/// and tested for UTF-8 — seller bots upload text as either.
fn is_binary_media_type(media_type: &str) -> bool {
    ["image/", "audio/", "video/", "font/"]
        .iter()
        .any(|p| media_type.starts_with(p))
        || matches!(
            media_type,
            "application/pdf"
                | "application/zip"
                | "application/gzip"
                | "application/x-tar"
                | "application/wasm"
        )
}
