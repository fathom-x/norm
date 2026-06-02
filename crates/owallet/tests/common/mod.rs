//! Shared harness for the owallet end-to-end tests (`tests/e2e.rs`).
//!
//! Replaces the deleted Python pytest E2E suite
//! (`owallet/wallet_mcp/tests/`): it boots a real Rails marketplace as a
//! subprocess (with the `/test_only` seed harness enabled), spawns a live
//! `owallet serve` against it, and drives owallet over its `/mcp`
//! JSON-RPC endpoint — exercising the owallet ↔ marketplace ↔ bot_manager
//! seams end-to-end, no chain involved.
//!
//! These tests need a prepared Postgres test DB for both the main app and
//! bot_manager (`bin/rails db:test:prepare`), so they are `#[ignore]`d and
//! run only in the dedicated `owallet-rs-e2e` CI workflow, never in the
//! plain `cargo test --workspace`.

#![allow(dead_code)]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use owallet_crypto::{npub_from_private_key, Address, PrivateKey};
use owallet_db::Database;
use serde_json::{json, Value};

/// Throwaway key shared by every test so the owallet identity (EVM
/// address + npub) is stable. Matches the value the old pytest harness
/// hard-coded.
pub const TEST_PRIVKEY: &str = "0x4c0883a69102937d6231471b5dbb6204fe512961708279e2a5e6f0a4d1f3c2bb";
pub const DB_PASSWORD: &str = "test-password-12345";
pub const WALLET_PASSWORD: &str = "test-password-12345-wallet";

const BOOT_TIMEOUT: Duration = Duration::from_secs(120);

/// Repo root: `CARGO_MANIFEST_DIR` is `.../owallet-rs/crates/owallet`, so
/// the marketplace root is three levels up.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repo root three levels above the crate")
        .to_path_buf()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}

fn http_client(timeout: Duration) -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("build reqwest client")
}

/// Poll `url` until it answers with an accepted status, or panic after
/// `BOOT_TIMEOUT`. `accept_redirect` allows any 2xx/3xx (owallet's `/`
/// answers with a redirect to `/wallet`); otherwise only 200 counts.
fn wait_http(url: &str, accept_redirect: bool, what: &str) {
    let client = http_client(Duration::from_secs(2));
    let deadline = Instant::now() + BOOT_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(resp) = client.get(url).send() {
            let code = resp.status().as_u16();
            if code == 200 || (accept_redirect && (200..400).contains(&code)) {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    panic!("{what} did not become ready at {url} within {BOOT_TIMEOUT:?}");
}

fn post_json(url: &str, body: Value) -> Value {
    let client = http_client(Duration::from_secs(20));
    let resp = client
        .post(url)
        .json(&body)
        .send()
        .unwrap_or_else(|e| panic!("POST {url}: {e}"));
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    assert!(status.is_success(), "POST {url} -> {status}: {text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("POST {url} bad JSON: {e}\n{text}"))
}

// ---------------------------------------------------------------------------
// Rails marketplace subprocess + the /test_only seed harness
// ---------------------------------------------------------------------------

pub struct RailsServer {
    child: Child,
    pub base_url: String,
}

impl RailsServer {
    /// Boot `bin/rails server` (test env, TEST_HARNESS on) and wait on `/up`.
    /// Assumes `bin/rails db:test:prepare` has already run (CI does this).
    pub fn start() -> Self {
        let port = free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let secret = std::env::var("SECRET_KEY_BASE").unwrap_or_else(|_| "x".repeat(64));
        let child = Command::new("bin/rails")
            .current_dir(repo_root())
            .args([
                "server",
                "-p",
                &port.to_string(),
                "-b",
                "127.0.0.1",
                "-e",
                "test",
            ])
            .env("RAILS_ENV", "test")
            .env("TEST_HARNESS", "1")
            .env("GATEWAY_API_SECRET", "test-gateway-secret")
            .env("SECRET_KEY_BASE", secret)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn `bin/rails server` (is bin/rails on the repo root?)");
        wait_http(&format!("{base_url}/up"), false, "rails server");
        Self { child, base_url }
    }

    pub fn seed(&self, path: &str, body: Value) -> Value {
        post_json(&format!("{}/test_only/{}", self.base_url, path), body)
    }

    pub fn run_job(&self, job: &str) -> Value {
        self.seed("run_job", json!({ "job": job }))
    }
}

impl Drop for RailsServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Run bot_manager's real seller poll+deliver runner against the live
/// marketplace and return the delivered order ids. Mirrors the pytest
/// `fulfill_as_seller` fixture.
pub fn fulfill_as_seller(
    rails_base: &str,
    seller_token: &str,
    content: &str,
    echo_buyer_note_field: Option<&str>,
) -> Vec<String> {
    let mut cmd = Command::new("bin/rails");
    cmd.current_dir(repo_root().join("bots/bot_manager"))
        .args(["runner", "test/support/owallet_e2e_fulfill.rb"])
        .env("RAILS_ENV", "test")
        .env("OVERPAY_RAILS_URL", rails_base)
        .env("SELLER_TOKEN", seller_token)
        .env("DELIVER_CONTENT", content);
    if let Some(field) = echo_buyer_note_field {
        cmd.env("DELIVER_FROM_BUYER_NOTE_FIELD", field);
    }
    let out = cmd.output().expect("run owallet_e2e_fulfill.rb");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "bot fulfill runner failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let last = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("fulfill runner printed no JSON line:\n{stdout}"));
    let parsed: Value = serde_json::from_str(last.trim()).expect("fulfill JSON");
    parsed["delivered"]
        .as_array()
        .expect("delivered array")
        .iter()
        .map(|v| v.as_str().expect("delivered id is string").to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// owallet serve subprocess + MCP JSON-RPC client
// ---------------------------------------------------------------------------

pub struct OwalletServer {
    child: Child,
    _tmp: tempfile::TempDir,
    pub base_url: String,
    pub address: String,
    pub npub: String,
    mcp_token: String,
}

impl OwalletServer {
    /// Bootstrap a wallet DB (init + import the test key) and spawn
    /// `owallet serve` pointed at `rails_url`. When `rails_bearer` is
    /// supplied it is stored as the Overpay bearer for this wallet, so
    /// MCP tools authenticate to Rails as that user instead of falling
    /// back to NIP-98 (used by the credit-funded flows).
    pub fn start(rails_url: &str, rails_bearer: Option<&str>) -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let db_path = tmp.path().join("wallet.db");
        // An explicit `--config` file stops serve from auto-loading a
        // scaffolded `prod.owallet` (which would clobber OVERPAY_RAILS_URL);
        // the file itself carries the Rails URL serve reads.
        let config_path = tmp.path().join("test.owallet");
        std::fs::write(&config_path, format!("OVERPAY_RAILS_URL={rails_url}\n")).unwrap();

        let bin = env!("CARGO_BIN_EXE_owallet");
        let with_env = |cmd: &mut Command| {
            cmd.env("OWALLET_PASSWORD", DB_PASSWORD)
                .env("OWALLET_WALLET_PASSWORD", WALLET_PASSWORD)
                .env("OWALLET_DB_PATH", &db_path)
                .env("OWALLET_CONFIG_DIR", tmp.path());
        };

        let mut init = Command::new(bin);
        with_env(&mut init);
        init.args(["--config", config_path.to_str().unwrap(), "init"]);
        run_ok(&mut init, "owallet init");

        let mut import = Command::new(bin);
        with_env(&mut import);
        import.args([
            "--config",
            config_path.to_str().unwrap(),
            "import",
            "--private-key",
            TEST_PRIVKEY,
        ]);
        run_ok(&mut import, "owallet import");

        let sk = PrivateKey::from_hex(TEST_PRIVKEY).expect("parse test key");
        let address = Address::from_private_key(&sk).to_hex_lower();
        let npub = npub_from_private_key(&sk).expect("derive npub");

        let port = free_port();
        let base_url = format!("http://127.0.0.1:{port}");
        let mcp_token = format!("e2e_mcp_{port}");

        // Seed tokens into the (not-yet-serving) DB on a private connection.
        {
            let mut db = Database::open(&db_path).expect("open wallet db");
            assert!(db.unlock(DB_PASSWORD).expect("unlock"), "wrong db password");
            db.write_access_token(
                &mcp_token,
                "e2e",
                &["wallet".to_string()],
                None,
                Some(&npub),
            )
            .expect("mint MCP access token");
            if let Some(bearer) = rails_bearer {
                // The MCP state keys the Overpay bearer under its issuer URL,
                // which serve derives as `http://127.0.0.1:<port>`.
                db.write_token(&npub, &base_url, bearer, "overpay-oauth")
                    .expect("store Overpay bearer");
            }
        }

        let mut serve = Command::new(bin);
        with_env(&mut serve);
        serve
            .args([
                "--config",
                config_path.to_str().unwrap(),
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = serve.spawn().expect("spawn owallet serve");

        wait_http(&format!("{base_url}/"), true, "owallet serve");

        Self {
            child,
            _tmp: tmp,
            base_url,
            address,
            npub,
            mcp_token,
        }
    }

    /// Invoke an MCP tool over `/mcp` and return its `structuredContent`.
    /// Panics if the tool result is an error.
    pub fn call_tool(&self, name: &str, arguments: Value) -> Value {
        let client = http_client(Duration::from_secs(30));
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        let resp = client
            .post(format!("{}/mcp", self.base_url))
            .bearer_auth(&self.mcp_token)
            .json(&body)
            .send()
            .unwrap_or_else(|e| panic!("POST /mcp {name}: {e}"));
        let v: Value = resp.json().expect("mcp response JSON");
        let result = &v["result"];
        assert_eq!(
            result["isError"], false,
            "tool {name} returned isError: {}",
            result["content"][0]["text"]
        );
        result["structuredContent"].clone()
    }
}

impl Drop for OwalletServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run_ok(cmd: &mut Command, what: &str) {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("{what}: spawn failed: {e}"));
    assert!(
        out.status.success(),
        "{what} failed ({}):\n{}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
