//! Hashed API keys for the localhost OpenAI-compatible provider.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderKeyRow {
    pub id: String,
    pub npub: String,
    pub created_at: i64,
    /// Who minted it — "dashboard" or "browser login". `None` on rows from
    /// before the column existed.
    pub label: Option<String>,
    /// The first few characters of the raw key (`owk_` + 8 hex), stored so
    /// the dashboard list is matchable against a key the user holds. Short
    /// enough to reveal nothing useful of the 256-bit token.
    pub token_prefix: Option<String>,
}

pub(crate) fn insert(
    conn: &Connection,
    id: &str,
    token_hash: &str,
    npub: &str,
    created_at: i64,
    label: &str,
    token_prefix: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO provider_keys(id, token_hash, npub, created_at, label, token_prefix) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, token_hash, npub, created_at, label, token_prefix],
    )?;
    Ok(())
}

pub(crate) fn read_npub(conn: &Connection, token_hash: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT npub FROM provider_keys WHERE token_hash = ?1",
            params![token_hash],
            |row| row.get(0),
        )
        .optional()?)
}

pub(crate) fn list(conn: &Connection, npub: &str) -> Result<Vec<ProviderKeyRow>> {
    let mut statement = conn.prepare(
        "SELECT id, npub, created_at, label, token_prefix FROM provider_keys \
         WHERE npub = ?1 ORDER BY created_at DESC",
    )?;
    let rows = statement.query_map(params![npub], |row| {
        Ok(ProviderKeyRow {
            id: row.get(0)?,
            npub: row.get(1)?,
            created_at: row.get(2)?,
            label: row.get(3)?,
            token_prefix: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(crate) fn delete(conn: &Connection, id: &str, npub: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM provider_keys WHERE id = ?1 AND npub = ?2",
        params![id, npub],
    )?;
    Ok(())
}
