//! `GET|POST /wallet/login`, `POST /wallet/logout`.

use askama::Template;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;

use super::{attach_cookie, current_session, session_clear_cookie, session_set_cookie};
use crate::error::AppError;
use crate::session::SessionRole;
use crate::state::AppState;
use crate::templates::LoginTemplate;

pub async fn login_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if current_session(&state.sessions, &headers).is_some() {
        return Ok(Redirect::to("/wallet").into_response());
    }
    let tpl = LoginTemplate {
        role: "wallet",
        identifier: String::new(),
        error: None,
    };
    Ok(Html(tpl.render()?).into_response())
}

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub identifier: String,
    #[serde(default)]
    pub password: String,
}

pub async fn login_post(
    State(state): State<AppState>,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let role_str = if form.role == "admin" {
        "admin"
    } else {
        "wallet"
    };

    let db = state
        .db
        .lock()
        .map_err(|e| AppError::Internal(format!("db mutex poisoned: {e}")))?;

    let resolved_role = if role_str == "admin" {
        if db.verify_password(&form.password)? {
            Some(SessionRole::Admin)
        } else {
            None
        }
    } else {
        match db.find_wallet_by_identifier(&form.identifier)? {
            Some(npub) => {
                if db.verify_wallet_password(&npub, &form.password)? {
                    Some(SessionRole::Wallet { npub })
                } else {
                    None
                }
            }
            None => None,
        }
    };

    let Some(role) = resolved_role else {
        let tpl = LoginTemplate {
            role: if role_str == "admin" {
                "admin"
            } else {
                "wallet"
            },
            identifier: form.identifier,
            error: Some("Invalid credentials.".into()),
        };
        return Ok((StatusCode::UNAUTHORIZED, Html(tpl.render()?)).into_response());
    };

    let token = state.sessions.insert(role);
    let mut headers = HeaderMap::new();
    attach_cookie(&mut headers, session_set_cookie(&token));
    Ok((headers, Redirect::to("/wallet")).into_response())
}

pub async fn logout_post(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if let Some(raw) = headers.get(axum::http::header::COOKIE) {
        if let Ok(raw) = raw.to_str() {
            for c in cookie::Cookie::split_parse(raw).flatten() {
                if c.name() == crate::state::SESSION_COOKIE {
                    state.sessions.remove(c.value());
                }
            }
        }
    }
    let mut hmap = HeaderMap::new();
    attach_cookie(&mut hmap, session_clear_cookie());
    Ok((hmap, Redirect::to("/wallet/login")).into_response())
}
