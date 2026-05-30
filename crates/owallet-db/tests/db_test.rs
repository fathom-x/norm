//! Port of `wallet_mcp/test_db.py` — 32 cases covering the encrypted SQLite
//! layer. Each test uses a fresh tempfile DB.

use std::path::PathBuf;

use owallet_db::{Database, DbError};
use tempfile::TempDir;

const MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon \
     abandon abandon abandon abandon abandon about";
const NPUB: &str = "npub1testabcdef1234567890";
const TOKEN: &str = "tok_test_abcdef123456";
const HOST: &str = "overpay-eykm.onrender.com";

struct TestDb {
    db: Database,
    path: PathBuf,
    // Holding the TempDir keeps the directory alive for the test's lifetime.
    _tmp: TempDir,
}

fn fresh(password: &str) -> TestDb {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("test_owallet.db");
    let db = Database::init(&path, password).expect("init");
    TestDb {
        db,
        path,
        _tmp: tmp,
    }
}

fn fresh_path() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("test_owallet.db");
    (tmp, path)
}

// ---------------------------------------------------------------------------
// Init and lifecycle
// ---------------------------------------------------------------------------

#[test]
fn db_not_exists_before_init() {
    let (_tmp, path) = fresh_path();
    assert!(!Database::exists(&path));
}

#[test]
fn init_creates_db() {
    let t = fresh("testpass123");
    assert!(Database::exists(&t.path));
}

#[test]
fn init_raises_if_already_exists() {
    let t = fresh("testpass123");
    drop(t.db);
    match Database::init(&t.path, "testpass123") {
        Err(DbError::AlreadyExists(_)) => {}
        Err(e) => panic!("expected AlreadyExists, got {e:?}"),
        Ok(_) => panic!("expected error, got Ok"),
    }
}

#[test]
fn unlock_correct_password() {
    let mut t = fresh("correctpass");
    t.db.lock();
    assert!(!t.db.is_unlocked());
    assert!(t.db.unlock("correctpass").unwrap());
    assert!(t.db.is_unlocked());
}

#[test]
fn unlock_wrong_password() {
    let mut t = fresh("correctpass");
    t.db.lock();
    assert!(!t.db.unlock("wrongpass").unwrap());
    assert!(!t.db.is_unlocked());
}

#[test]
fn unlock_raises_if_no_db() {
    let (_tmp, path) = fresh_path();
    match Database::open(&path) {
        Err(DbError::NotFound(_)) => {}
        Err(e) => panic!("expected NotFound, got {e:?}"),
        Ok(_) => panic!("expected error, got Ok"),
    }
}

#[test]
fn lock_wipes_key() {
    let mut t = fresh("testpass123");
    assert!(t.db.is_unlocked());
    t.db.lock();
    assert!(!t.db.is_unlocked());
}

#[test]
fn verify_password_correct() {
    let t = fresh("mypassword");
    assert!(t.db.verify_password("mypassword").unwrap());
}

#[test]
fn verify_password_wrong() {
    let t = fresh("mypassword");
    assert!(!t.db.verify_password("notmypassword").unwrap());
}

#[test]
fn verify_password_does_not_change_lock_state() {
    let mut t = fresh("mypassword");
    t.db.lock();
    let _ = t.db.verify_password("mypassword").unwrap();
    assert!(!t.db.is_unlocked());
}

// ---------------------------------------------------------------------------
// Wallet CRUD
// ---------------------------------------------------------------------------

#[test]
fn write_and_read_wallet_mnemonic() {
    let t = fresh("pw");
    t.db.write_wallet(NPUB, MNEMONIC, None).unwrap();
    assert_eq!(t.db.read_seed(NPUB).unwrap(), Some(MNEMONIC.into()));
}

#[test]
fn wallet_roundtrip_survives_lock_unlock() {
    let mut t = fresh("pw");
    t.db.write_wallet(NPUB, MNEMONIC, None).unwrap();
    t.db.lock();
    t.db.unlock("pw").unwrap();
    assert_eq!(t.db.read_seed(NPUB).unwrap(), Some(MNEMONIC.into()));
}

#[test]
fn read_seed_returns_none_for_missing() {
    let t = fresh("pw");
    assert!(t.db.read_seed("npub1nonexistent").unwrap().is_none());
}

#[test]
fn write_wallet_overwrites_existing() {
    let t = fresh("pw");
    t.db.write_wallet(NPUB, MNEMONIC, None).unwrap();
    let new_seed = format!("0x{}", "ab".repeat(32));
    t.db.write_wallet(NPUB, &new_seed, None).unwrap();
    assert_eq!(t.db.read_seed(NPUB).unwrap(), Some(new_seed));
}

#[test]
fn delete_wallet_removes() {
    let t = fresh("pw");
    t.db.write_wallet(NPUB, MNEMONIC, None).unwrap();
    t.db.delete_wallet(NPUB).unwrap();
    assert!(t.db.read_seed(NPUB).unwrap().is_none());
}

#[test]
fn list_wallets_returns_npubs() {
    let t = fresh("pw");
    t.db.write_wallet("npub1aaa", MNEMONIC, None).unwrap();
    t.db.write_wallet("npub1bbb", MNEMONIC, None).unwrap();
    let wallets = t.db.list_wallets().unwrap();
    let npubs: Vec<&str> = wallets.iter().map(|w| w.npub.as_str()).collect();
    assert!(npubs.contains(&"npub1aaa"));
    assert!(npubs.contains(&"npub1bbb"));
}

#[test]
fn read_all_seeds_returns_all() {
    let t = fresh("pw");
    let alt = format!("0x{}", "ab".repeat(32));
    t.db.write_wallet("npub1aaa", MNEMONIC, None).unwrap();
    t.db.write_wallet("npub1bbb", &alt, None).unwrap();
    let seeds: std::collections::HashMap<String, String> =
        t.db.read_all_seeds().unwrap().into_iter().collect();
    assert_eq!(seeds.get("npub1aaa").map(String::as_str), Some(MNEMONIC));
    assert_eq!(seeds.get("npub1bbb"), Some(&alt));
}

#[test]
fn read_all_seeds_raises_when_locked() {
    let mut t = fresh("pw");
    t.db.write_wallet(NPUB, MNEMONIC, None).unwrap();
    t.db.lock();
    let err = t.db.read_all_seeds().unwrap_err();
    assert!(matches!(err, DbError::Locked));
}

// ---------------------------------------------------------------------------
// Default npub
// ---------------------------------------------------------------------------

#[test]
fn write_and_read_default_npub() {
    let t = fresh("pw");
    t.db.write_default_npub(NPUB).unwrap();
    assert_eq!(t.db.read_default_npub().unwrap().as_deref(), Some(NPUB));
}

#[test]
fn read_default_npub_returns_none_when_unset() {
    let t = fresh("pw");
    assert!(t.db.read_default_npub().unwrap().is_none());
}

// ---------------------------------------------------------------------------
// Bearer token CRUD
// ---------------------------------------------------------------------------

#[test]
fn write_and_read_token() {
    let t = fresh("pw");
    t.db.write_token(NPUB, HOST, TOKEN, "owallet").unwrap();
    assert_eq!(t.db.read_token(NPUB, HOST).unwrap().as_deref(), Some(TOKEN));
}

#[test]
fn token_roundtrip_survives_lock_unlock() {
    let mut t = fresh("pw");
    t.db.write_token(NPUB, HOST, TOKEN, "owallet").unwrap();
    t.db.lock();
    t.db.unlock("pw").unwrap();
    assert_eq!(t.db.read_token(NPUB, HOST).unwrap().as_deref(), Some(TOKEN));
}

#[test]
fn read_token_returns_none_for_missing() {
    let t = fresh("pw");
    assert!(t.db.read_token(NPUB, "unknownhost.com").unwrap().is_none());
}

#[test]
fn write_token_overwrites() {
    let t = fresh("pw");
    t.db.write_token(NPUB, HOST, TOKEN, "owallet").unwrap();
    t.db.write_token(NPUB, HOST, "newtoken_xyz", "owallet")
        .unwrap();
    assert_eq!(
        t.db.read_token(NPUB, HOST).unwrap().as_deref(),
        Some("newtoken_xyz")
    );
}

#[test]
fn delete_token_removes() {
    let t = fresh("pw");
    t.db.write_token(NPUB, HOST, TOKEN, "owallet").unwrap();
    t.db.delete_token(NPUB, HOST).unwrap();
    assert!(t.db.read_token(NPUB, HOST).unwrap().is_none());
}

// ---------------------------------------------------------------------------
// Tamper detection
// ---------------------------------------------------------------------------

#[test]
fn tampered_ciphertext_raises() {
    let t = fresh("pw");
    t.db.write_wallet(NPUB, MNEMONIC, None).unwrap();

    // Corrupt the ciphertext directly via a fresh sqlite connection to the
    // same file (the Database handle keeps its own connection open).
    let conn = rusqlite::Connection::open(&t.path).unwrap();
    let mut corrupted: Vec<u8> = conn
        .query_row(
            "SELECT encrypted_seed FROM wallets WHERE npub = ?1",
            rusqlite::params![NPUB],
            |row| row.get(0),
        )
        .unwrap();
    corrupted[0] ^= 0xFF;
    conn.execute(
        "UPDATE wallets SET encrypted_seed = ?1 WHERE npub = ?2",
        rusqlite::params![corrupted, NPUB],
    )
    .unwrap();
    drop(conn);

    let err = t.db.read_seed(NPUB).unwrap_err();
    assert!(matches!(err, DbError::Decrypt(_)));
}

// ---------------------------------------------------------------------------
// OAuth client / code / token CRUD
// ---------------------------------------------------------------------------

#[test]
fn write_and_read_oauth_client() {
    let t = fresh("pw");
    t.db.write_oauth_client(
        "client_abc",
        None,
        &["http://localhost:1234/callback".into()],
        &["authorization_code".into()],
        None,
        None,
    )
    .unwrap();
    let row = t.db.read_oauth_client("client_abc").unwrap().unwrap();
    assert_eq!(row.client_id, "client_abc");
    assert_eq!(row.client_secret, None);
    assert_eq!(row.redirect_uris, vec!["http://localhost:1234/callback"]);
}

#[test]
fn read_oauth_client_missing() {
    let t = fresh("pw");
    assert!(t.db.read_oauth_client("does_not_exist").unwrap().is_none());
}

#[test]
fn write_and_read_auth_code() {
    let t = fresh("pw");
    let expires_at = now_secs_f64() + 300.0;
    t.db.write_auth_code(
        "code_xyz",
        "client_abc",
        &["wallet".into()],
        "challenge123",
        "http://localhost:1234/callback",
        true,
        expires_at,
        None,
    )
    .unwrap();
    let row = t.db.read_auth_code("code_xyz").unwrap().unwrap();
    assert_eq!(row.code, "code_xyz");
    assert_eq!(row.scopes, vec!["wallet"]);
    assert!(row.redirect_uri_provided_explicitly);
}

#[test]
fn delete_auth_code_removes() {
    let t = fresh("pw");
    t.db.write_auth_code(
        "code_del",
        "client_abc",
        &["wallet".into()],
        "ch",
        "http://localhost/cb",
        false,
        now_secs_f64() + 300.0,
        None,
    )
    .unwrap();
    t.db.delete_auth_code("code_del").unwrap();
    assert!(t.db.read_auth_code("code_del").unwrap().is_none());
}

#[test]
fn write_and_read_access_token() {
    let t = fresh("pw");
    t.db.write_access_token(
        "tok_access_abc",
        "client_abc",
        &["wallet".into()],
        None,
        None,
    )
    .unwrap();
    let row = t.db.read_access_token("tok_access_abc").unwrap().unwrap();
    assert_eq!(row.token, "tok_access_abc");
    assert_eq!(row.scopes, vec!["wallet"]);
    assert!(row.expires_at.is_none());
}

#[test]
fn revoke_access_token_removes() {
    let t = fresh("pw");
    t.db.write_access_token("tok_revoke", "client_abc", &["wallet".into()], None, None)
        .unwrap();
    t.db.revoke_access_token("tok_revoke").unwrap();
    assert!(t.db.read_access_token("tok_revoke").unwrap().is_none());
}

fn now_secs_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ---------------------------------------------------------------------------
// Purchase cache — port of test_db.py purchase tests
// ---------------------------------------------------------------------------

use serde_json::json;

#[test]
fn upsert_and_read_purchase_round_trips_snapshot() {
    let t = fresh("pw");
    let order = json!({
        "order_id": "ord_abc",
        "product_title": "Tokyo Weather Report",
        "seller_username": "weather_bot",
        "status": "paid",
        "fulfillment_status": "delivered",
        "delivered_at": 1700000000_i64,
        "paid_at": 1699999000_i64,
        "total_usd_cents": 5,
        "delivered_content": "{\"description\":\"sunny\",\"temperature_f\":72,\"image\":\"AAA\"}",
        "delivered_content_type": "application/json",
        "listing": {
            "id": "listing_xyz",
            "delivered_content_schema": {
                "properties": {
                    "description": {"title": "Description"},
                    "temperature_f": {"title": "Temp"},
                    "image": {"x-widget": "image"},
                }
            },
        },
    });
    assert_eq!(
        t.db.upsert_purchase("npub1abc", &order).unwrap(),
        Some("ord_abc".to_string())
    );

    let record = t.db.read_purchase("npub1abc", "ord_abc").unwrap().unwrap();
    assert_eq!(record.title.as_deref(), Some("Tokyo Weather Report"));
    assert_eq!(record.seller.as_deref(), Some("weather_bot"));
    assert_eq!(record.fulfillment_status.as_deref(), Some("delivered"));
    assert_eq!(record.delivered_at, Some(1700000000));
    assert_eq!(record.total_usd_cents, Some(5));
    assert_eq!(
        record.delivered_content_type.as_deref(),
        Some("application/json")
    );
    assert!(record
        .delivered_content
        .as_deref()
        .unwrap()
        .contains("temperature_f"));
    assert_eq!(
        record.delivered_content_schema.as_ref().unwrap()["properties"]["image"]["x-widget"],
        "image"
    );
    assert_eq!(record.snapshot["product_title"], "Tokyo Weather Report");
}

#[test]
fn upsert_purchase_idempotent_and_refreshes_status() {
    let t = fresh("pw");
    t.db.upsert_purchase(
        "npub1",
        &json!({"order_id": "o1", "fulfillment_status": "awaiting_seller"}),
    )
    .unwrap();
    t.db.upsert_purchase(
        "npub1",
        &json!({"order_id": "o1", "fulfillment_status": "delivered", "delivered_content": "hi"}),
    )
    .unwrap();
    let record = t.db.read_purchase("npub1", "o1").unwrap().unwrap();
    assert_eq!(record.fulfillment_status.as_deref(), Some("delivered"));
    assert_eq!(record.delivered_content.as_deref(), Some("hi"));
    assert_eq!(t.db.count_purchases("npub1").unwrap(), 1);
}

#[test]
fn purchases_scoped_by_npub() {
    let t = fresh("pw");
    t.db.upsert_purchase(
        "npub_a",
        &json!({"order_id": "o1", "fulfillment_status": "delivered"}),
    )
    .unwrap();
    t.db.upsert_purchase(
        "npub_b",
        &json!({"order_id": "o2", "fulfillment_status": "delivered"}),
    )
    .unwrap();
    assert_eq!(t.db.count_purchases("npub_a").unwrap(), 1);
    assert_eq!(t.db.count_purchases("npub_b").unwrap(), 1);
    let ids: Vec<String> =
        t.db.list_purchases("npub_a", 50, 0, None)
            .unwrap()
            .into_iter()
            .map(|p| p.order_id)
            .collect();
    assert_eq!(ids, vec!["o1"]);
    assert!(t.db.read_purchase("npub_a", "o2").unwrap().is_none());
}

#[test]
fn list_purchases_orders_by_delivered_desc_and_filters() {
    let t = fresh("pw");
    t.db.upsert_purchase(
        "n",
        &json!({"order_id": "old", "fulfillment_status": "delivered", "delivered_at": 1000}),
    )
    .unwrap();
    t.db.upsert_purchase(
        "n",
        &json!({"order_id": "new", "fulfillment_status": "delivered", "delivered_at": 2000}),
    )
    .unwrap();
    t.db.upsert_purchase(
        "n",
        &json!({"order_id": "fail", "fulfillment_status": "failed", "delivered_at": 1500}),
    )
    .unwrap();

    let ids: Vec<String> =
        t.db.list_purchases("n", 50, 0, None)
            .unwrap()
            .into_iter()
            .map(|p| p.order_id)
            .collect();
    assert_eq!(ids[0], "new");
    assert!(ids[1] == "fail" || ids[1] == "old");

    let delivered_only: Vec<String> =
        t.db.list_purchases("n", 50, 0, Some("delivered"))
            .unwrap()
            .into_iter()
            .map(|p| p.order_id)
            .collect();
    assert_eq!(delivered_only, vec!["new", "old"]);
}

#[test]
fn upsert_purchase_returns_none_without_id() {
    let t = fresh("pw");
    assert_eq!(
        t.db.upsert_purchase("n", &json!({"fulfillment_status": "delivered"}))
            .unwrap(),
        None
    );
    assert_eq!(t.db.count_purchases("n").unwrap(), 0);
}

#[test]
fn delete_purchase_removes_it() {
    let t = fresh("pw");
    t.db.upsert_purchase(
        "n",
        &json!({"order_id": "o1", "fulfillment_status": "delivered"}),
    )
    .unwrap();
    t.db.delete_purchase("n", "o1").unwrap();
    assert!(t.db.read_purchase("n", "o1").unwrap().is_none());
    assert_eq!(t.db.count_purchases("n").unwrap(), 0);
}

#[test]
fn upsert_purchase_coerces_iso8601_timestamps() {
    // Rails serializes Time attributes as ISO-8601 strings; coerce to unix.
    let t = fresh("pw");
    t.db.upsert_purchase(
        "n",
        &json!({"order_id": "o1", "fulfillment_status": "delivered",
                "delivered_at": "2023-11-14T22:13:20Z"}),
    )
    .unwrap();
    let record = t.db.read_purchase("n", "o1").unwrap().unwrap();
    assert_eq!(record.delivered_at, Some(1700000000));
}

#[test]
fn purchase_table_added_to_existing_db_via_migrate() {
    // An older DB without the purchases table should gain it on next open.
    let (_tmp, path) = fresh_path();
    drop(Database::init(&path, "pw").unwrap());
    {
        // Simulate a legacy DB: drop the table + index the migration adds.
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute("DROP INDEX IF EXISTS idx_purchases_npub_cached", [])
            .unwrap();
        conn.execute("DROP TABLE purchases", []).unwrap();
    }
    // Re-open runs migrate(), which re-creates the table.
    let mut db = Database::open(&path).unwrap();
    assert!(db.unlock("pw").unwrap());
    assert_eq!(
        db.upsert_purchase(
            "n",
            &json!({"order_id": "o1", "fulfillment_status": "delivered"})
        )
        .unwrap(),
        Some("o1".to_string())
    );
    assert_eq!(db.count_purchases("n").unwrap(), 1);
}
