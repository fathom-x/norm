//! Errors.

use serde_json::{Map, Value};

use crate::generated::ErrorCode;

/// Everything an SDK call can fail with.
///
/// The `Display` text of each variant is stable (callers surface it), and
/// an API error prints as `HTTP <status>: <response body>` — the body as
/// [`describe_body`] shortens it.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The API answered with an error status.
    #[error("{0}")]
    Api(#[from] ApiError),
    /// The request never got an HTTP answer (DNS, TLS, connection, timeout).
    /// Carries reqwest's error: reqwest is part of this crate's API.
    #[error("http transport: {0}")]
    Transport(#[from] reqwest::Error),
    /// A success response that isn't the expected JSON.
    #[error("response is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("url parse: {0}")]
    Url(#[from] url::ParseError),
    /// An auth header couldn't be built: a token with a newline in it, or
    /// a NIP-98 signer whose key is not a valid secp256k1 secret key.
    #[error("nip98 sign: {0}")]
    Sign(String),
    /// A delivered file couldn't be retrieved or wasn't usable.
    #[error("delivered content: {0}")]
    Delivery(String),
}

impl Error {
    /// The API error, when this is one.
    #[must_use]
    pub fn api(&self) -> Option<&ApiError> {
        match self {
            Self::Api(e) => Some(e),
            _ => None,
        }
    }

    /// The HTTP status of an API error.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        self.api().map(|e| e.status)
    }

    /// The API error's machine-readable code, when it has one.
    #[must_use]
    pub fn code(&self) -> Option<&ErrorCode> {
        self.api().and_then(|e| e.code.as_ref())
    }

    /// True for an API error meaning the buyer's credits don't cover
    /// something (see [`ApiError::is_insufficient_credits`]).
    #[must_use]
    pub fn is_insufficient_credits(&self) -> bool {
        self.api().is_some_and(ApiError::is_insufficient_credits)
    }
}

/// An error response from the API: `{"error": "...", "code": "...", ...}`.
///
/// It prints as `HTTP <status>: <body>`, with the body shortened by
/// [`describe_body`]; [`Self::body`] keeps it whole.
#[non_exhaustive]
#[derive(Debug, Clone, thiserror::Error)]
#[error("HTTP {status}: {}", describe_body(*.status, .body))]
pub struct ApiError {
    pub status: u16,
    /// The `code` field. `None` from servers that predate error codes, or
    /// for responses that aren't API errors (a proxy's HTML page).
    pub code: Option<ErrorCode>,
    /// The `error` field: the human-readable message.
    pub message: Option<String>,
    /// Every other field of the error body (e.g. `remaining_cents`).
    pub details: Map<String, Value>,
    /// The raw response body.
    pub body: String,
}

impl ApiError {
    /// Parse an error response. Never fails: a body that isn't the API's
    /// error JSON leaves `code`/`message` empty and keeps the raw text.
    #[must_use]
    pub fn from_response(status: u16, body: &[u8]) -> Self {
        let text = String::from_utf8_lossy(body).into_owned();
        let mut details = match serde_json::from_slice::<Value>(body) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        };
        let message = match details.remove("error") {
            Some(Value::String(s)) => Some(s),
            Some(other) => {
                details.insert("error".into(), other);
                None
            }
            None => None,
        };
        let code = match details.remove("code") {
            Some(Value::String(s)) => Some(ErrorCode::from(s.as_str())),
            Some(other) => {
                details.insert("code".into(), other);
                None
            }
            None => None,
        };
        Self {
            status,
            code,
            message,
            details,
            body: text,
        }
    }

    /// True for errors that mean "the buyer's credits don't cover this":
    /// `insufficient_credits` / `no_credits`. For servers that predate error
    /// codes it falls back to a 402 or 422 whose body mentions credits.
    #[must_use]
    pub fn is_insufficient_credits(&self) -> bool {
        match &self.code {
            Some(ErrorCode::InsufficientCredits | ErrorCode::NoCredits) => true,
            Some(_) => false,
            None => {
                matches!(self.status, 402 | 422)
                    && self.body.to_ascii_lowercase().contains("credits")
            }
        }
    }

    /// A numeric detail field, e.g. `remaining_cents`.
    #[must_use]
    pub fn detail_f64(&self, key: &str) -> Option<f64> {
        self.details.get(key).and_then(Value::as_f64)
    }
}

/// Longest response body an error message carries verbatim. Overpay's own
/// errors are a line of JSON; anything longer is not something to print.
const BODY_MAX: usize = 2000;

/// A response body as an error message should show it.
///
/// A failed request is not always answered by Overpay. A firewall or proxy
/// in front of it answers with a whole web page, and relaying that verbatim
/// once put 221 KB of HTML (inline fonts included) into a chat. An HTML body
/// is reduced to a sentence naming the page's heading; any other body is cut
/// at 2000 bytes, on a character boundary.
#[must_use]
pub fn describe_body(status: u16, body: &str) -> String {
    let lower = body.to_ascii_lowercase();
    if lower.contains("<!doctype html") || lower.contains("<html") {
        let heading = tag_text(body, &lower, "h1").or_else(|| tag_text(body, &lower, "title"));
        let mut message = String::from("the server answered with a web page instead of a reply");
        if let Some(heading) = heading {
            message.push_str(&format!(" (\"{heading}\")"));
        }
        message.push('.');
        if status == 403 {
            message.push_str(
                " The request was blocked before it reached Overpay. Sending the same \
                 request again is likely to be blocked again.",
            );
        }
        return message;
    }
    if body.len() <= BODY_MAX {
        return body.to_string();
    }
    let mut end = BODY_MAX;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} more bytes)", &body[..end], body.len() - end)
}

/// The text of the first `<tag>` element, whitespace collapsed, if it is
/// short enough to be a heading. `lower` is `body` lowercased (ASCII only,
/// so byte offsets match).
fn tag_text(body: &str, lower: &str, tag: &str) -> Option<String> {
    let open = lower.find(&format!("<{tag}"))?;
    let start = open + lower[open..].find('>')? + 1;
    let end = start + lower[start..].find(&format!("</{tag}"))?;
    let text = body[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty() && text.len() <= 120 && !text.contains('<')).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(status: u16, body: &str) -> String {
        Error::from(ApiError::from_response(status, body.as_bytes())).to_string()
    }

    #[test]
    fn json_error_bodies_are_shown_as_they_are() {
        assert_eq!(
            message(422, r#"{"error":"No available credits for this seller"}"#),
            r#"HTTP 422: {"error":"No available credits for this seller"}"#
        );
        assert_eq!(message(500, ""), "HTTP 500: ");
    }

    #[test]
    fn a_firewall_block_page_becomes_one_sentence() {
        let page = format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head><title>Blocked</title>\
             <style>@font-face {{ src: url(\"data:font/woff2;base64,{}\") }}</style></head>\n\
             <body><h1 class=\"type-heading-04\">403 -\n Forbidden</h1></body></html>",
            "A".repeat(200_000)
        );
        assert_eq!(
            message(403, &page),
            "HTTP 403: the server answered with a web page instead of a reply \
             (\"403 - Forbidden\"). The request was blocked before it reached Overpay. \
             Sending the same request again is likely to be blocked again."
        );
    }

    #[test]
    fn other_web_pages_name_their_heading_or_title() {
        assert_eq!(
            message(
                502,
                "<html><head><title>Bad Gateway</title></head><body>nginx</body></html>"
            ),
            "HTTP 502: the server answered with a web page instead of a reply (\"Bad Gateway\")."
        );
        assert_eq!(
            message(503, "<HTML><BODY>down</BODY></HTML>"),
            "HTTP 503: the server answered with a web page instead of a reply."
        );
    }

    #[test]
    fn long_bodies_are_cut_on_a_character_boundary() {
        let body = "é".repeat(1500); // 3000 bytes, 2 per character
        assert_eq!(
            message(500, &body),
            format!("HTTP 500: {}… (1000 more bytes)", "é".repeat(1000))
        );
    }

    #[test]
    fn the_body_itself_is_kept_whole_for_callers() {
        let page = format!("<html>{}</html>", "x".repeat(10_000));
        assert_eq!(ApiError::from_response(403, page.as_bytes()).body, page);
    }
}
