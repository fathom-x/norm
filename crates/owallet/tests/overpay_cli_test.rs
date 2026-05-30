//! End-to-end tests for the Overpay-backed CLI subcommands.
//!
//! Each test spins up a `wiremock` server that pretends to be the Rails
//! app, points `OVERPAY_RAILS_URL` at it, and drives the compiled binary
//! via `assert_cmd`.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::Duration;

use assert_cmd::cargo::cargo_bin;
use assert_cmd::Command;
use predicates::str::contains;
use serde_json::json;
use tempfile::TempDir;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ABANDON_12: &str = "abandon abandon abandon abandon abandon abandon \
     abandon abandon abandon abandon abandon about";

fn init_and_import(db_path: &Path) {
    Command::cargo_bin("owallet")
        .unwrap()
        .env("OWALLET_DB_PATH", db_path)
        .env("OWALLET_PASSWORD", "pw")
        .arg("init")
        .assert()
        .success();
    Command::cargo_bin("owallet")
        .unwrap()
        .env("OWALLET_DB_PATH", db_path)
        .env("OWALLET_PASSWORD", "pw")
        // `import` now prompts for a per-wallet password; supply it
        // non-interactively for the test harness.
        .env("OWALLET_WALLET_PASSWORD", "wallet-pw")
        .arg("import")
        .arg("--mnemonic")
        .arg(ABANDON_12)
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// list marketplace
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_marketplace_pretty_prints_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/listings"))
        .and(query_param("category", "books"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"id": "L1", "title": "Hello World", "seller_slug": "alice", "price_usd": 9.99},
                {"id": "L2", "title": "Another",     "seller_slug": "bob",   "price_usd": 19.00},
            ],
            "next_cursor": "cur_next"
        })))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let server_uri = server.uri();
    let db_path = tmp.path().join("test.db");
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("owallet")
            .unwrap()
            .env("OWALLET_DB_PATH", &db_path)
            .env("OWALLET_PASSWORD", "pw")
            .env("OVERPAY_RAILS_URL", &server_uri)
            .arg("list")
            .arg("marketplace")
            .arg("--category")
            .arg("books")
            .arg("--limit")
            .arg("10")
            .assert()
            .success()
            .stdout(contains("Hello World"))
            .stdout(contains("Another"))
            .stdout(contains("Next page: --cursor cur_next"));
    })
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------
// account with a stored token
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_fetches_overpay_info_when_token_present() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "username": "alice",
            "account_number": "1234567890123456",
            "email": "alice@example.com",
        })))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let server_uri = server.uri();

    let db_path2 = db_path.clone();
    let server_uri2 = server_uri.clone();
    tokio::task::spawn_blocking(move || {
        init_and_import(&db_path2);

        // Inject a bearer token directly via the DB API.
        let mut db = owallet_db::Database::open(&db_path2).unwrap();
        assert!(db.unlock("pw").unwrap());
        let npub = db.read_default_npub().unwrap().unwrap();
        let host = server_uri2.trim_end_matches('/').to_string();
        db.write_token(&npub, &host, "tok_abc", "overpay-oauth")
            .unwrap();
    })
    .await
    .unwrap();

    let db_path3 = db_path.clone();
    let server_uri3 = server_uri.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("owallet")
            .unwrap()
            .env("OWALLET_DB_PATH", &db_path3)
            .env("OWALLET_PASSWORD", "pw")
            .env("OVERPAY_RAILS_URL", &server_uri3)
            .arg("account")
            .assert()
            .success()
            .stdout(contains("Default wallet"))
            .stdout(contains("Linked Overpay account"))
            .stdout(contains("alice"))
            .stdout(contains("1234567890123456"))
            .stdout(contains("alice@example.com"));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_falls_back_to_nip98_when_no_token() {
    // No Bearer stored → the `account` command must sign the live fetch
    // with a NIP-98 envelope using the wallet key.
    let server = MockServer::start().await;
    use wiremock::matchers::header_regex;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .and(header_regex("authorization", r"^Nostr .+$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "username": "alice-via-nip98",
            "account_number": "1234567890123456",
        })))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let db_path2 = db_path.clone();
    let server_uri = server.uri();
    tokio::task::spawn_blocking(move || {
        init_and_import(&db_path2);
        Command::cargo_bin("owallet")
            .unwrap()
            .env("OWALLET_DB_PATH", &db_path2)
            .env("OWALLET_PASSWORD", "pw")
            .env("OVERPAY_RAILS_URL", &server_uri)
            .arg("account")
            .assert()
            .success()
            .stdout(contains("Default wallet"))
            .stdout(contains("Linked Overpay account"))
            .stdout(contains("NIP-98"))
            .stdout(contains("alice-via-nip98"));
    })
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------
// authorize (PKCE end-to-end against a fake Rails)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authorize_drives_full_pkce_flow_against_fake_rails() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth/clients"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "client_id": "client_test_id",
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "tok_test",
            "token_type": "Bearer",
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "username": "alice",
            "account_number": "1234567890123456",
        })))
        .mount(&server)
        .await;

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let server_uri = server.uri();

    // Seed a wallet.
    let db_path2 = db_path.clone();
    tokio::task::spawn_blocking(move || init_and_import(&db_path2))
        .await
        .unwrap();

    // Run the binary as a manual subprocess so we can read its stdout
    // line by line, extract the printed authorize URL, and deliver the
    // callback ourselves. (Real users would let the browser do this.)
    let mut child = std::process::Command::new(cargo_bin("owallet"))
        .env("OWALLET_DB_PATH", &db_path)
        .env("OWALLET_PASSWORD", "pw")
        .env("OVERPAY_RAILS_URL", &server_uri)
        // Suppress any actual browser opening; on Linux the `open` crate
        // ultimately shells out to xdg-open which we don't want to run.
        .env("BROWSER", "/bin/true")
        .arg("authorize")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Read stdout until we see the printed http(s) URL.
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut accum = String::new();
    let mut auth_url: Option<String> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        let mut line = String::new();
        let n = tokio::task::block_in_place(|| reader.read_line(&mut line)).unwrap_or(0);
        if n == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }
        accum.push_str(&line);
        let trimmed = line.trim();
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            auth_url = Some(trimmed.to_string());
            break;
        }
    }
    let auth_url =
        auth_url.unwrap_or_else(|| panic!("authorize URL not printed; stdout so far:\n{accum}"));

    let parsed = url::Url::parse(&auth_url).unwrap();
    let pairs: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
    let state = pairs.get("state").cloned().expect("state in URL");
    let redirect_uri = pairs
        .get("redirect_uri")
        .cloned()
        .expect("redirect_uri in URL");

    // Deliver the callback to the binary's local server.
    let cb = url::Url::parse_with_params(
        &redirect_uri,
        &[("code", "THE_CODE"), ("state", state.as_str())],
    )
    .unwrap();
    let resp = reqwest::Client::new()
        .get(cb.as_str())
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "callback responded {}",
        resp.status()
    );

    // Drain the rest of stdout while the process exits.
    let mut remaining = String::new();
    tokio::task::block_in_place(|| {
        let _ = reader.read_to_string(&mut remaining);
    });
    accum.push_str(&remaining);

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "binary failed.\nstdout:\n{accum}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let full_stdout = format!("{accum}{}", String::from_utf8_lossy(&output.stdout));
    assert!(full_stdout.contains("Authorized"), "stdout: {full_stdout}");
    assert!(full_stdout.contains("alice"), "stdout: {full_stdout}");

    // Confirm the token landed in the DB.
    let host = server_uri.trim_end_matches('/').to_string();
    let mut db = owallet_db::Database::open(&db_path).unwrap();
    assert!(db.unlock("pw").unwrap());
    let npub = db.read_default_npub().unwrap().unwrap();
    let token = db.read_token(&npub, &host).unwrap();
    assert_eq!(token.as_deref(), Some("tok_test"));
}

// ---------------------------------------------------------------------------
// login (needs a stored token)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_errors_without_token() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let db_path2 = db_path.clone();
    tokio::task::spawn_blocking(move || {
        init_and_import(&db_path2);
        Command::cargo_bin("owallet")
            .unwrap()
            .env("OWALLET_DB_PATH", &db_path2)
            .env("OWALLET_PASSWORD", "pw")
            .arg("login")
            .assert()
            .failure()
            .stderr(contains("not authorized"));
    })
    .await
    .unwrap();
}

// Needed for the std::io::Read trait on BufReader's underlying stdout.
use std::io::Read as _;
