//! Schema definition and incremental migrations.
//!
//! `create` runs once on init; `migrate` runs every time a DB is opened and
//! attempts additive `ALTER TABLE` statements (matching `db.py::_migrate`).
//! Each migration is wrapped in a `try`-style block and "duplicate column"
//! errors are swallowed, so applying twice is idempotent.

use rusqlite::Connection;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS wallets (
    npub           TEXT PRIMARY KEY,
    encrypted_seed BLOB NOT NULL,
    nonce          BLOB NOT NULL,
    created_at     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS tokens (
    npub            TEXT NOT NULL,
    host            TEXT NOT NULL,
    encrypted_token BLOB NOT NULL,
    nonce           BLOB NOT NULL,
    token_name      TEXT,
    stored_at       INTEGER NOT NULL,
    PRIMARY KEY (npub, host)
);

CREATE TABLE IF NOT EXISTS oauth_clients (
    client_id                    TEXT PRIMARY KEY,
    client_secret                TEXT,
    redirect_uris                TEXT NOT NULL,
    grant_types                  TEXT NOT NULL,
    scope                        TEXT,
    token_endpoint_auth_method   TEXT,
    registered_at                INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS auth_codes (
    code                           TEXT PRIMARY KEY,
    client_id                      TEXT NOT NULL,
    scopes                         TEXT NOT NULL,
    code_challenge                 TEXT NOT NULL,
    redirect_uri                   TEXT NOT NULL,
    redirect_uri_provided_explicitly INTEGER NOT NULL,
    expires_at                     REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS access_tokens (
    token      TEXT PRIMARY KEY,
    client_id  TEXT NOT NULL,
    scopes     TEXT NOT NULL,
    expires_at INTEGER
);

CREATE TABLE IF NOT EXISTS purchases (
    npub                   TEXT NOT NULL,
    order_id               TEXT NOT NULL,
    listing_id             TEXT,
    title                  TEXT,
    seller                 TEXT,
    payment_status         TEXT,
    fulfillment_status     TEXT,
    delivered_at           INTEGER,
    paid_at                INTEGER,
    total_usd_cents        INTEGER,
    delivered_content      TEXT,
    delivered_content_type TEXT,
    schema_json            TEXT,
    snapshot_json          TEXT NOT NULL,
    cached_at              INTEGER NOT NULL,
    PRIMARY KEY (npub, order_id)
);

CREATE INDEX IF NOT EXISTS idx_purchases_npub_cached ON purchases(npub, cached_at DESC);
"#;

const MIGRATIONS: &[&str] = &[
    "ALTER TABLE oauth_clients ADD COLUMN scope TEXT",
    "ALTER TABLE oauth_clients ADD COLUMN token_endpoint_auth_method TEXT",
    "ALTER TABLE wallets ADD COLUMN last_accessed INTEGER",
    "ALTER TABLE auth_codes ADD COLUMN npub TEXT",
    "ALTER TABLE access_tokens ADD COLUMN npub TEXT",
    "ALTER TABLE wallets ADD COLUMN wallet_password_hash TEXT",
    "ALTER TABLE wallets ADD COLUMN address TEXT",
    "ALTER TABLE wallets ADD COLUMN overpay_username TEXT",
    // Purchase cache — added via migration so existing DBs gain it on open
    // (mirrors the standalone DDL in `wallet_mcp/db.py::_migrate`). Both are
    // `IF NOT EXISTS`, so re-running is a no-op.
    "CREATE TABLE IF NOT EXISTS purchases (
        npub                   TEXT NOT NULL,
        order_id               TEXT NOT NULL,
        listing_id             TEXT,
        title                  TEXT,
        seller                 TEXT,
        payment_status         TEXT,
        fulfillment_status     TEXT,
        delivered_at           INTEGER,
        paid_at                INTEGER,
        total_usd_cents        INTEGER,
        delivered_content      TEXT,
        delivered_content_type TEXT,
        schema_json            TEXT,
        snapshot_json          TEXT NOT NULL,
        cached_at              INTEGER NOT NULL,
        PRIMARY KEY (npub, order_id)
    )",
    "CREATE INDEX IF NOT EXISTS idx_purchases_npub_cached ON purchases(npub, cached_at DESC)",
];

pub(crate) fn create(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)?;
    migrate(conn)
}

pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    for ddl in MIGRATIONS {
        match conn.execute(ddl, []) {
            Ok(_) => {}
            // "duplicate column name: X" is expected when the column already
            // exists. rusqlite reports it as SqliteFailure with extended code
            // 1 ("SQLITE_ERROR" — generic SQL error). We can't filter on the
            // extended code reliably across versions, so we filter on the
            // error message instead.
            Err(rusqlite::Error::SqliteFailure(_, Some(msg)))
                if msg.contains("duplicate column name") => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
