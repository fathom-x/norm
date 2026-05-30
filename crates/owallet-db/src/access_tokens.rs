//! `access_tokens` table — MCP-client bearer tokens issued by the local AS.
//! `expires_at` is nullable (non-expiring tokens; valid until server restart).

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessTokenRow {
    pub token: String,
    pub client_id: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<i64>,
    pub npub: Option<String>,
}

pub(crate) fn insert(
    conn: &Connection,
    token: &str,
    client_id: &str,
    scopes: &[String],
    expires_at: Option<i64>,
    npub: Option<&str>,
) -> Result<()> {
    let scopes_json = serde_json::to_string(scopes)?;
    conn.execute(
        "INSERT INTO access_tokens(token, client_id, scopes, expires_at, npub)
         VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(token) DO NOTHING",
        params![token, client_id, scopes_json, expires_at, npub],
    )?;
    Ok(())
}

pub(crate) fn read(conn: &Connection, token: &str) -> Result<Option<AccessTokenRow>> {
    let row = conn
        .query_row(
            "SELECT token, client_id, scopes, expires_at, npub
             FROM access_tokens WHERE token = ?1",
            params![token],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;

    let Some((token, client_id, scopes_json, expires_at, npub)) = row else {
        return Ok(None);
    };

    Ok(Some(AccessTokenRow {
        token,
        client_id,
        scopes: serde_json::from_str(&scopes_json)?,
        expires_at,
        npub,
    }))
}

pub(crate) fn delete(conn: &Connection, token: &str) -> Result<()> {
    conn.execute("DELETE FROM access_tokens WHERE token = ?1", params![token])?;
    Ok(())
}
