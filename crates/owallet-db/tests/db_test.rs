//! Port of `wallet_mcp/test_db.py` — 32 cases covering the encrypted SQLite
//! layer. Each test uses a fresh tempfile DB.

use std::path::PathBuf;

use owallet_db::{Database, DbError};
use tempfile::TempDir;

const MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon \
     abandon abandon abandon abandon abandon about";
const NPUB: &str = "npub1testabcdef1234567890";
const TOKEN: &str = "tok_test_abcdef123456";
const HOST: &str = "overpay.example.com";

struct TestDb {
    db: Database,
    path: PathBuf,
    // Holding the TempDir keeps the directory alive for the test's lifetime.
    _tmp: TempDir,
}

fn fresh(password: &str) -> TestDb {
    let tmp = TempDir::new().expect("tempdir");
    let path = tmp.path().join("test_owallet.db");
    // The per-wallet data dir (order cache, wallet state) co-locates with the
    // DB file by default — here `<tmp>/test_owallet/` — so tests are isolated
    // from the real ~/.owallet with no extra setup.
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
fn zcash_address_roundtrip() {
    let t = fresh("pw");
    t.db.write_wallet(NPUB, MNEMONIC, None).unwrap();
    assert_eq!(t.db.read_zcash_address(NPUB).unwrap(), None);
    let ua = "u1exampleorchardunifiedaddress";
    t.db.write_zcash_address(NPUB, ua).unwrap();
    assert_eq!(t.db.read_zcash_address(NPUB).unwrap(), Some(ua.into()));
    // Surfaced on the wallet row too.
    let row =
        t.db.list_wallets()
            .unwrap()
            .into_iter()
            .find(|w| w.npub == NPUB)
            .unwrap();
    assert_eq!(row.zcash_address.as_deref(), Some(ua));
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

#[test]
fn read_token_migrating_refiles_from_a_legacy_host() {
    let (_tmp, path) = fresh_path();
    let mut db = Database::init(&path, "pw").unwrap();
    assert!(db.unlock("pw").unwrap());
    db.write_token("npub1", "http://legacy.test", "tok", "overpay-oauth")
        .unwrap();

    let legacy = vec!["http://legacy.test".to_string()];
    assert_eq!(
        db.read_token_migrating("npub1", "http://canonical.test", &legacy)
            .unwrap()
            .as_deref(),
        Some("tok")
    );
    // Re-filed, not copied — a second read hits the canonical key directly.
    assert_eq!(
        db.read_token("npub1", "http://canonical.test")
            .unwrap()
            .as_deref(),
        Some("tok")
    );
    assert_eq!(db.read_token("npub1", "http://legacy.test").unwrap(), None);
}

#[test]
fn read_token_migrating_prefers_the_canonical_row() {
    let (_tmp, path) = fresh_path();
    let mut db = Database::init(&path, "pw").unwrap();
    assert!(db.unlock("pw").unwrap());
    db.write_token("npub1", "http://canonical.test", "fresh", "overpay-oauth")
        .unwrap();
    db.write_token("npub1", "http://legacy.test", "stale", "overpay-oauth")
        .unwrap();

    let legacy = vec!["http://legacy.test".to_string()];
    assert_eq!(
        db.read_token_migrating("npub1", "http://canonical.test", &legacy)
            .unwrap()
            .as_deref(),
        Some("fresh")
    );
    // The stale row is left alone rather than clobbering the live one.
    assert_eq!(
        db.read_token("npub1", "http://legacy.test")
            .unwrap()
            .as_deref(),
        Some("stale")
    );
}

#[test]
fn read_token_migrating_returns_none_when_nothing_is_stored() {
    let (_tmp, path) = fresh_path();
    let mut db = Database::init(&path, "pw").unwrap();
    assert!(db.unlock("pw").unwrap());
    let legacy = vec!["http://legacy.test".to_string()];
    assert_eq!(
        db.read_token_migrating("npub1", "http://canonical.test", &legacy)
            .unwrap(),
        None
    );
}

#[test]
fn provider_keys_are_wallet_scoped_and_revocable() {
    let t = fresh("pw");
    let db = &t.db;
    let (row, key) = db
        .create_provider_key(NPUB, "dashboard", owallet_db::PROVIDER_SCOPE_CHAT, None)
        .unwrap();

    assert!(key.starts_with("owk_"));
    assert_eq!(row.label.as_deref(), Some("dashboard"));
    assert_eq!(row.token_prefix.as_deref(), Some(&key[..12]));
    assert_eq!(
        db.read_provider_key_npub(&key).unwrap().as_deref(),
        Some(NPUB)
    );
    assert_eq!(db.list_provider_keys(NPUB).unwrap(), vec![row.clone()]);

    db.delete_provider_key(&row.id, NPUB).unwrap();
    assert_eq!(db.read_provider_key_npub(&key).unwrap(), None);
    assert!(db.list_provider_keys(NPUB).unwrap().is_empty());
}

#[test]
fn provider_key_scopes_gate_spending() {
    let t = fresh("pw");
    let db = &t.db;

    let (chat_row, chat_key) = db
        .create_provider_key(NPUB, "dashboard", owallet_db::PROVIDER_SCOPE_CHAT, None)
        .unwrap();
    let (spend_row, spend_key) = db
        .create_provider_key(NPUB, "dashboard", "chat spend", None)
        .unwrap();

    assert!(!chat_row.can_spend());
    assert!(spend_row.can_spend());

    let auth = db.read_provider_key_auth(&chat_key).unwrap().unwrap();
    assert_eq!(auth.npub, NPUB);
    assert!(!auth.can_spend());

    let auth = db.read_provider_key_auth(&spend_key).unwrap().unwrap();
    assert!(auth.can_spend());

    // A row from before the scopes column existed (NULL scopes) must stay
    // chat-only — pre-existing keys never silently gain spending power.
    assert!(!owallet_db::scopes_allow_spend(None));
}

#[test]
fn provider_key_budget_reserve_release_record_lifecycle() {
    use owallet_db::BudgetReservation;

    let t = fresh("pw");
    let db = &t.db;

    let (key, _) = db
        .create_provider_key(NPUB, "dashboard", "chat spend", Some(1000))
        .unwrap();
    assert_eq!(key.daily_budget_usd_cents, Some(1000));
    assert_eq!(key.spent_today_usd_cents(), 0);
    assert_eq!(key.remaining_today_usd_cents(), Some(1000));

    // Reserve within budget; a second reservation over the remainder
    // refuses atomically and reports the numbers.
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 600).unwrap(),
        BudgetReservation::Reserved
    );
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 500).unwrap(),
        BudgetReservation::OverBudget {
            daily_budget_usd_cents: 1000,
            remaining_today_usd_cents: 400,
        }
    );

    // A released reservation restores allowance; the retry then fits.
    db.release_provider_key_spend(&key.id, 600).unwrap();
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 500).unwrap(),
        BudgetReservation::Reserved
    );

    // After-the-fact recording may overshoot; remaining floors at 0 and
    // everything refuses from then on.
    db.record_provider_key_spend(&key.id, 700).unwrap();
    let row = db.read_provider_key(&key.id).unwrap().unwrap();
    assert_eq!(row.spent_today_usd_cents(), 1200);
    assert_eq!(row.remaining_today_usd_cents(), Some(0));
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 1).unwrap(),
        BudgetReservation::OverBudget {
            daily_budget_usd_cents: 1000,
            remaining_today_usd_cents: 0,
        }
    );

    // Raising the budget takes effect immediately and keeps today's spend.
    assert!(db
        .update_provider_key_budget(&key.id, NPUB, Some(2000))
        .unwrap());
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 800).unwrap(),
        BudgetReservation::Reserved
    );
    // Clearing it makes the key unlimited; spend is still tracked.
    assert!(db.update_provider_key_budget(&key.id, NPUB, None).unwrap());
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 1_000_000)
            .unwrap(),
        BudgetReservation::Reserved
    );
    let row = db.read_provider_key(&key.id).unwrap().unwrap();
    assert_eq!(row.daily_budget_usd_cents, None);
    assert_eq!(row.remaining_today_usd_cents(), None);
    assert_eq!(row.spent_today_usd_cents(), 1_002_000);

    // Budget edits are wallet-scoped like delete.
    assert!(!db
        .update_provider_key_budget(&key.id, "npub1someoneelse", Some(1))
        .unwrap());

    // A revoked key refuses reservations by name.
    db.delete_provider_key(&key.id, NPUB).unwrap();
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 1).unwrap(),
        BudgetReservation::KeyMissing
    );
}

#[test]
fn provider_key_budget_window_resets_at_the_day_boundary() {
    use owallet_db::BudgetReservation;

    let t = fresh("pw");
    let db = &t.db;
    let (key, _) = db
        .create_provider_key(NPUB, "dashboard", "chat spend", Some(1000))
        .unwrap();

    // Exhaust today's budget.
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 1000).unwrap(),
        BudgetReservation::Reserved
    );
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 1).unwrap(),
        BudgetReservation::OverBudget {
            daily_budget_usd_cents: 1000,
            remaining_today_usd_cents: 0,
        }
    );

    // Backdate the window to yesterday (second connection, like the
    // corruption test) — as if the wallet-local midnight passed since the
    // spend.
    let conn = rusqlite::Connection::open(&t.path).unwrap();
    conn.execute(
        "UPDATE provider_keys SET spent_day = spent_day - 1 WHERE id = ?1",
        [&key.id],
    )
    .unwrap();
    drop(conn);

    // Reads report a fresh window without any write happening…
    let row = db.read_provider_key(&key.id).unwrap().unwrap();
    assert_eq!(row.spent_today_usd_cents(), 0);
    assert_eq!(row.remaining_today_usd_cents(), Some(1000));

    // …and the reserve UPDATE rolls the row over to today lazily.
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 900).unwrap(),
        BudgetReservation::Reserved
    );
    let row = db.read_provider_key(&key.id).unwrap().unwrap();
    assert_eq!(row.spent_today_usd_cents(), 900);
    assert_eq!(row.spent_day, Some(db.current_budget_day()));

    // A release for a reservation made before the rollover is a no-op —
    // it must not refund yesterday's spend into today's fresh window.
    let conn = rusqlite::Connection::open(&t.path).unwrap();
    conn.execute(
        "UPDATE provider_keys SET spent_day = spent_day - 1 WHERE id = ?1",
        [&key.id],
    )
    .unwrap();
    drop(conn);
    db.release_provider_key_spend(&key.id, 900).unwrap();
    // The API normalizes past-day spend to 0, so check the raw column.
    let conn = rusqlite::Connection::open(&t.path).unwrap();
    let raw: i64 = conn
        .query_row(
            "SELECT spent_usd_cents FROM provider_keys WHERE id = ?1",
            [&key.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        raw, 900,
        "yesterday's raw spend is untouched by a stale release"
    );
}

#[test]
fn timezone_setting_roundtrips_and_validates() {
    let t = fresh("pw");
    let db = &t.db;

    // Unset means UTC.
    assert_eq!(db.read_timezone().unwrap(), None);

    db.write_timezone("Europe/Berlin").unwrap();
    assert_eq!(
        db.read_timezone().unwrap().as_deref(),
        Some("Europe/Berlin")
    );
    db.write_timezone("UTC").unwrap();
    assert_eq!(db.read_timezone().unwrap().as_deref(), Some("UTC"));

    let err = db.write_timezone("Mars/Olympus_Mons").unwrap_err();
    assert!(err.to_string().contains("unknown IANA timezone"));
    assert_eq!(
        db.read_timezone().unwrap().as_deref(),
        Some("UTC"),
        "a rejected name must not clobber the stored setting"
    );

    assert!(owallet_db::timezone_is_valid("America/New_York"));
    assert!(!owallet_db::timezone_is_valid("America/Not_A_Place"));
}

#[test]
fn spend_cap_setting_roundtrips_and_clears() {
    let t = fresh("pw");
    let db = &t.db;
    assert_eq!(db.read_spend_cap_usd_cents().unwrap(), None);
    db.write_spend_cap_usd_cents(Some(500)).unwrap();
    assert_eq!(db.read_spend_cap_usd_cents().unwrap(), Some(500));
    db.write_spend_cap_usd_cents(None).unwrap();
    assert_eq!(db.read_spend_cap_usd_cents().unwrap(), None);
}

#[test]
fn budget_window_follows_the_wallet_timezone() {
    use owallet_db::BudgetReservation;

    let t = fresh("pw");
    let db = &t.db;
    let (key, _) = db
        .create_provider_key(NPUB, "dashboard", "chat spend", Some(1000))
        .unwrap();

    // Etc/GMT+12 (UTC-12) and Etc/GMT-14 (UTC+14) are 26 hours apart, so
    // their local calendar dates differ at every instant — switching
    // between them always lands in a different budget window, which makes
    // this deterministic regardless of when the test runs.
    db.write_timezone("Etc/GMT+12").unwrap();
    let west_day = db.current_budget_day();
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 1000).unwrap(),
        BudgetReservation::Reserved
    );
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 1).unwrap(),
        BudgetReservation::OverBudget {
            daily_budget_usd_cents: 1000,
            remaining_today_usd_cents: 0,
        }
    );

    db.write_timezone("Etc/GMT-14").unwrap();
    assert_ne!(
        db.current_budget_day(),
        west_day,
        "26h-apart zones never share a calendar date"
    );
    // The stored window belongs to another day now: reads show a fresh
    // budget and the next reserve rolls the row into the new window.
    let row = db.read_provider_key(&key.id).unwrap().unwrap();
    assert_eq!(row.spent_today_usd_cents(), 0);
    assert_eq!(row.remaining_today_usd_cents(), Some(1000));
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 1000).unwrap(),
        BudgetReservation::Reserved
    );
    let row = db.read_provider_key(&key.id).unwrap().unwrap();
    assert_eq!(row.spent_today_usd_cents(), 1000);
    assert_eq!(row.spent_day, Some(db.current_budget_day()));
}

#[test]
fn provider_key_rows_predating_the_budget_columns_read_as_unlimited_untouched() {
    let t = fresh("pw");
    let db = &t.db;
    let (key, raw) = db
        .create_provider_key(NPUB, "dashboard", "chat spend", Some(500))
        .unwrap();

    // Simulate a row from before the budget columns: NULLs everywhere
    // (via a second connection, like the corruption test above).
    let conn = rusqlite::Connection::open(&t.path).unwrap();
    conn.execute(
        "UPDATE provider_keys SET daily_budget_usd_cents = NULL, spent_usd_cents = NULL, \
         spent_day = NULL WHERE id = ?1",
        [&key.id],
    )
    .unwrap();
    drop(conn);

    let row = db.read_provider_key_auth(&raw).unwrap().unwrap();
    assert_eq!(
        row.daily_budget_usd_cents, None,
        "NULL budget means no limit"
    );
    assert_eq!(row.spent_today_usd_cents(), 0, "NULL spent reads as zero");
    assert_eq!(
        db.try_reserve_provider_key_spend(&key.id, 100).unwrap(),
        owallet_db::BudgetReservation::Reserved,
        "COALESCE in the guard must treat NULL spent as zero"
    );
    let row = db.read_provider_key(&key.id).unwrap().unwrap();
    assert_eq!(row.spent_today_usd_cents(), 100);
}
