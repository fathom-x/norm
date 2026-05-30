//! `/wallet/*` route handlers.

pub mod delete;
pub mod generate;
pub mod import;
pub mod index;
pub mod login;
pub mod oauth;
pub mod password;
pub mod purchases;
pub mod select;
pub mod send;

use axum::http::header::{HeaderMap, COOKIE, SET_COOKIE};
use axum::http::HeaderValue;
use axum::response::Redirect;
use cookie::{Cookie, SameSite};

use crate::session::{SessionStore, WebSession};
use crate::state::SESSION_COOKIE;

/// Look up the active session from the `Cookie` header(s). Browsers send a
/// single `Cookie:` header containing all cookies; `axum_test::TestServer`
/// sends one header per cookie. Iterate all values so both shapes work.
pub(crate) fn current_session(sessions: &SessionStore, headers: &HeaderMap) -> Option<WebSession> {
    for raw in headers.get_all(COOKIE).iter() {
        let Ok(s) = raw.to_str() else {
            continue;
        };
        for c in Cookie::split_parse(s).flatten() {
            if c.name() == SESSION_COOKIE {
                return sessions.get(c.value());
            }
        }
    }
    None
}

/// Build the `Set-Cookie` header value for `owallet_session=<token>`.
pub(crate) fn session_set_cookie(token: &str) -> HeaderValue {
    let mut c = Cookie::new(SESSION_COOKIE, token.to_string());
    c.set_path("/");
    c.set_http_only(true);
    c.set_same_site(SameSite::Lax);
    HeaderValue::from_str(&c.to_string()).expect("cookie string is always valid header value")
}

pub(crate) fn session_clear_cookie() -> HeaderValue {
    let mut c = Cookie::new(SESSION_COOKIE, "");
    c.set_path("/");
    c.set_max_age(cookie::time::Duration::ZERO);
    HeaderValue::from_str(&c.to_string()).expect("cookie")
}

/// Redirect to the login page when there is no live session.
pub(crate) fn redirect_to_login() -> Redirect {
    Redirect::to("/wallet/login")
}

/// Convenience: attach a `Set-Cookie` to a response header map.
pub(crate) fn attach_cookie(headers: &mut HeaderMap, value: HeaderValue) {
    headers.append(SET_COOKIE, value);
}
