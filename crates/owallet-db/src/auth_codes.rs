//! `auth_codes` table — PKCE authorization codes (TTL ~5min).

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthCodeRow {
    pub code: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    pub code_challenge: String,
    pub redirect_uri: String,
    pub redirect_uri_provided_explicitly: bool,
    pub expires_at: f64,
    pub npub: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn insert(
    conn: &Connection,
    code: &str,
    client_id: &str,
    scopes: &[String],
    code_challenge: &str,
    redirect_uri: &str,
    redirect_uri_provided_explicitly: bool,
    expires_at: f64,
    npub: Option<&str>,
) -> Result<()> {
    let scopes_json = serde_json::to_string(scopes)?;
    conn.execute(
        "INSERT INTO auth_codes(code, client_id, scopes, code_challenge, redirect_uri, redirect_uri_provided_explicitly, expires_at, npub)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(code) DO NOTHING",
        params![
            code,
            client_id,
            scopes_json,
            code_challenge,
            redirect_uri,
            redirect_uri_provided_explicitly as i64,
            expires_at,
            npub
        ],
    )?;
    Ok(())
}

pub(crate) fn read(conn: &Connection, code: &str) -> Result<Option<AuthCodeRow>> {
    let row = conn
        .query_row(
            "SELECT code, client_id, scopes, code_challenge, redirect_uri, redirect_uri_provided_explicitly, expires_at, npub
             FROM auth_codes WHERE code = ?1",
            params![code],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, f64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?;

    let Some((
        code,
        client_id,
        scopes_json,
        code_challenge,
        redirect_uri,
        explicit,
        expires_at,
        npub,
    )) = row
    else {
        return Ok(None);
    };

    Ok(Some(AuthCodeRow {
        code,
        client_id,
        scopes: serde_json::from_str(&scopes_json)?,
        code_challenge,
        redirect_uri,
        redirect_uri_provided_explicitly: explicit != 0,
        expires_at,
        npub,
    }))
}

pub(crate) fn delete(conn: &Connection, code: &str) -> Result<()> {
    conn.execute("DELETE FROM auth_codes WHERE code = ?1", params![code])?;
    Ok(())
}
