//! `wallets` table — encrypted seed storage + per-wallet metadata.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{DbError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WalletRow {
    pub npub: String,
    pub created_at: i64,
    pub last_accessed: Option<i64>,
    pub address: Option<String>,
    pub overpay_username: Option<String>,
}

pub(crate) fn insert(
    conn: &Connection,
    npub: &str,
    encrypted_seed: &[u8],
    nonce: &[u8],
    address: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO wallets(npub, encrypted_seed, nonce, created_at, address)
         VALUES(?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(npub) DO UPDATE SET
             encrypted_seed = excluded.encrypted_seed,
             nonce          = excluded.nonce,
             address        = COALESCE(excluded.address, address)",
        params![npub, encrypted_seed, nonce, now, address],
    )?;
    Ok(())
}

pub(crate) fn read_blob(conn: &Connection, npub: &str) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
    let row = conn
        .query_row(
            "SELECT encrypted_seed, nonce FROM wallets WHERE npub = ?1",
            params![npub],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?;
    Ok(row)
}

pub(crate) fn delete(conn: &Connection, npub: &str) -> Result<()> {
    conn.execute("DELETE FROM wallets WHERE npub = ?1", params![npub])?;
    Ok(())
}

pub(crate) fn list(conn: &Connection) -> Result<Vec<WalletRow>> {
    let mut stmt = conn.prepare(
        "SELECT npub, created_at, last_accessed, address, overpay_username
         FROM wallets
         ORDER BY created_at",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(WalletRow {
                npub: row.get(0)?,
                created_at: row.get(1)?,
                last_accessed: row.get(2)?,
                address: row.get(3)?,
                overpay_username: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(DbError::from)?;
    Ok(rows)
}

/// `(npub, ciphertext_with_tag, nonce)` tuple as stored in the wallets table.
pub(crate) type WalletBlob = (String, Vec<u8>, Vec<u8>);

pub(crate) fn list_blobs(conn: &Connection) -> Result<Vec<WalletBlob>> {
    let mut stmt = conn.prepare("SELECT npub, encrypted_seed, nonce FROM wallets")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub(crate) fn set_address(conn: &Connection, npub: &str, address: &str) -> Result<()> {
    conn.execute(
        "UPDATE wallets SET address = ?1 WHERE npub = ?2",
        params![address, npub],
    )?;
    Ok(())
}

pub(crate) fn set_username(conn: &Connection, npub: &str, username: &str) -> Result<()> {
    conn.execute(
        "UPDATE wallets SET overpay_username = ?1 WHERE npub = ?2",
        params![username, npub],
    )?;
    Ok(())
}

pub(crate) fn touch(conn: &Connection, npub: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE wallets SET last_accessed = ?1 WHERE npub = ?2",
        params![now, npub],
    )?;
    Ok(())
}

pub(crate) fn set_password_hash(conn: &Connection, npub: &str, hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE wallets SET wallet_password_hash = ?1 WHERE npub = ?2",
        params![hash, npub],
    )?;
    Ok(())
}

pub(crate) fn read_password_hash(conn: &Connection, npub: &str) -> Result<Option<String>> {
    let row = conn
        .query_row(
            "SELECT wallet_password_hash FROM wallets WHERE npub = ?1",
            params![npub],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(row.flatten())
}
