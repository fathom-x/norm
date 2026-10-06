//! Error types for the Overpay HTTP client.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum OverpayError {
    #[error("http transport: {0}")]
    Transport(#[from] reqwest::Error),
    /// `body` is kept whole for callers that inspect it; the message shows
    /// [`describe_body`]'s short form of it.
    #[error("HTTP {status}: {}", describe_body(*.status, .body))]
    HttpStatus { status: u16, body: String },
    #[error("response is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("url parse: {0}")]
    Url(#[from] url::ParseError),
    #[error("nip98 sign requires a private key for unauthenticated requests")]
    AuthRequired,
    #[error("nip98 sign: {0}")]
    Sign(String),
    /// A delivered file could not be retrieved or was not usable.
    #[error("delivered content: {0}")]
    Delivery(String),
}

/// Longest response body an error message carries verbatim. Overpay's own
/// errors are a line of JSON; anything longer is not something to print.
const BODY_MAX: usize = 2000;

/// The response body as an error message should show it.
///
/// A failed request is not always answered by Overpay. A firewall or proxy
/// in front of it answers with a whole web page, and relaying that verbatim
/// once put 221 KB of HTML (inline fonts included) into a chat. An HTML body
/// is reduced to a sentence naming the page's heading; any other body is cut
/// at [`BODY_MAX`] bytes.
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
        OverpayError::HttpStatus {
            status,
            body: body.to_string(),
        }
        .to_string()
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
        let text = message(403, &page);
        assert_eq!(
            text,
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
        let text = message(500, &body);
        assert_eq!(
            text,
            format!("HTTP 500: {}… (1000 more bytes)", "é".repeat(1000))
        );
    }

    #[test]
    fn the_body_itself_is_kept_whole_for_callers() {
        let page = format!("<html>{}</html>", "x".repeat(10_000));
        let OverpayError::HttpStatus { body, .. } = (OverpayError::HttpStatus {
            status: 403,
            body: page.clone(),
        }) else {
            unreachable!()
        };
        assert_eq!(body, page);
    }
}
