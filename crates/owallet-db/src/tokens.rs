//! `tokens` table — encrypted bearer tokens per (npub, host).

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenRow {
    pub npub: String,
    pub host: String,
    pub token_name: Option<String>,
    pub stored_at: i64,
}

pub(crate) fn insert(
    conn: &Connection,
    npub: &str,
    host: &str,
    encrypted_token: &[u8],
    nonce: &[u8],
    token_name: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO tokens(npub, host, encrypted_token, nonce, token_name, stored_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(npub, host) DO UPDATE SET
             encrypted_token = excluded.encrypted_token,
             nonce           = excluded.nonce,
             token_name      = excluded.token_name,
             stored_at       = excluded.stored_at",
        params![npub, host, encrypted_token, nonce, token_name, now],
    )?;
    Ok(())
}

pub(crate) fn read_blob(
    conn: &Connection,
    npub: &str,
    host: &str,
) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
    let row = conn
        .query_row(
            "SELECT encrypted_token, nonce FROM tokens WHERE npub = ?1 AND host = ?2",
            params![npub, host],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?;
    Ok(row)
}

pub(crate) fn delete(conn: &Connection, npub: &str, host: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM tokens WHERE npub = ?1 AND host = ?2",
        params![npub, host],
    )?;
    Ok(())
}
