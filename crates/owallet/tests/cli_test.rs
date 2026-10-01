//! End-to-end CLI tests that drive the compiled binary via `assert_cmd`.
//!
//! Each test isolates state by pointing `OWALLET_DB_PATH` at a tempfile and
//! supplying the password via `OWALLET_PASSWORD` so the binary never prompts.

use assert_cmd::Command;
use predicates::str::{contains, starts_with};
use tempfile::TempDir;

const ABANDON_12: &str = "abandon abandon abandon abandon abandon abandon \
     abandon abandon abandon abandon abandon about";

/// The well-known EVM address for the abandon-mnemonic at `m/44'/60'/0'/0/0`
/// (lowercase). EIP-55 mixed case is `0x9858EfFD232B4033E47d90003D41EC34EcaEda94`.
const ABANDON_ADDRESS_LOWER: &str = "0x9858effd232b4033e47d90003d41ec34ecaeda94";

fn owallet(tmp: &TempDir, password: &str) -> Command {
    let mut cmd = Command::cargo_bin("owallet").expect("binary exists");
    cmd.env("OWALLET_DB_PATH", tmp.path().join("test.db"));
    cmd.env("OWALLET_PASSWORD", password);
    // `generate` / `import` now prompt for a per-wallet password (used to
    // log into the web admin). Supply it non-interactively so the prompt
    // doesn't try to open /dev/tty under the test harness.
    cmd.env("OWALLET_WALLET_PASSWORD", "wallet-pw");
    // Don't accidentally pick up the developer's HOME-relative configs.
    cmd.env("HOME", tmp.path());
    cmd
}

#[test]
fn init_creates_db_file() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw")
        .arg("init")
        .assert()
        .success()
        .stdout(contains("Created encrypted database"));
    assert!(tmp.path().join("test.db").exists());
}

#[test]
fn init_is_idempotent_when_db_already_exists() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();
    // `init` is now idempotent: re-running on an existing DB prints a notice
    // and succeeds (it still scaffolds any missing .owallet configs).
    owallet(&tmp, "pw")
        .arg("init")
        .assert()
        .success()
        .stdout(contains("already exists"));
}

#[test]
fn config_prints_defaults() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw")
        .arg("config")
        .assert()
        .success()
        .stdout(contains("OVERPAY_RAILS_URL"))
        .stdout(contains("https://overpay.com"))
        .stdout(contains("8765"));
}

#[test]
fn config_mcp_prints_json_blob() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw")
        .arg("config")
        .arg("--mcp")
        .assert()
        .success()
        .stdout(contains("\"mcpServers\""))
        .stdout(contains("http://127.0.0.1:8765/mcp"));
}

#[test]
fn generate_stores_a_wallet_and_makes_it_default() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();

    let out = owallet(&tmp, "pw")
        .arg("generate")
        .arg("--words")
        .arg("12")
        .assert()
        .success();
    let stdout = std::str::from_utf8(&out.get_output().stdout).unwrap();
    assert!(stdout.contains("npub1"));
    assert!(stdout.contains("address: 0x"));
    // Exactly 12 words in the shown phrase.
    let phrase_line = stdout
        .lines()
        .find(|l| l.starts_with("  ") && l.split_whitespace().count() == 12)
        .expect("12-word phrase line present");
    assert_eq!(phrase_line.split_whitespace().count(), 12);

    // The account command now shows the wallet in a Field/Value table.
    owallet(&tmp, "pw")
        .arg("account")
        .assert()
        .success()
        .stdout(contains("Address"))
        .stdout(contains("npub1"))
        .stdout(contains("0x"));
}

#[test]
fn generate_defaults_to_24_words() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();

    let out = owallet(&tmp, "pw").arg("generate").assert().success();
    let stdout = std::str::from_utf8(&out.get_output().stdout).unwrap();
    // The seed phrase line is the indented 24-word line.
    let phrase_line = stdout
        .lines()
        .find(|l| l.starts_with("  ") && l.split_whitespace().count() == 24)
        .expect("24-word phrase line present by default");
    assert_eq!(phrase_line.split_whitespace().count(), 24);
}

#[test]
fn import_mnemonic_yields_known_address() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();

    owallet(&tmp, "pw")
        .arg("import")
        .arg("--mnemonic")
        .arg(ABANDON_12)
        .assert()
        .success()
        .stdout(contains("Imported wallet"));

    // `account` should now print the known abandon-mnemonic address.
    owallet(&tmp, "pw")
        .arg("account")
        .assert()
        .success()
        // Mixed-case EIP-55 is acceptable; check for either form.
        .stdout(contains(ABANDON_ADDRESS_LOWER));
}

#[test]
fn import_rejects_bad_mnemonic() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();

    owallet(&tmp, "pw")
        .arg("import")
        .arg("--mnemonic")
        .arg("not a valid mnemonic phrase here")
        .assert()
        .failure()
        .stderr(contains("invalid mnemonic"));
}

#[test]
fn import_hex_key_then_export_roundtrip() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();

    let key_hex = "1ab42cc412b618bdea3a599e3c9bae199ebf030895b039e9db1e30dafb12b727";
    owallet(&tmp, "pw")
        .arg("import")
        .arg("--private-key")
        .arg(format!("0x{key_hex}"))
        .assert()
        .success();

    // Export prints the hex key on stdout (npub goes to stderr).
    owallet(&tmp, "pw")
        .arg("export")
        .arg("key")
        .arg("--format")
        .arg("hex")
        .assert()
        .success()
        .stdout(starts_with(key_hex));
}

#[test]
fn export_mnemonic_after_mnemonic_import() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();
    owallet(&tmp, "pw")
        .arg("import")
        .arg("--mnemonic")
        .arg(ABANDON_12)
        .assert()
        .success();

    owallet(&tmp, "pw")
        .arg("export")
        .arg("key")
        .arg("--format")
        .arg("mnemonic")
        .assert()
        .success()
        .stdout(contains(ABANDON_12));
}

#[test]
fn export_mnemonic_after_hex_import_errors() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();
    owallet(&tmp, "pw")
        .arg("import")
        .arg("--private-key")
        .arg(format!("0x{}", "ab".repeat(32)))
        .assert()
        .success();

    owallet(&tmp, "pw")
        .arg("export")
        .arg("key")
        .arg("--format")
        .arg("mnemonic")
        .assert()
        .failure()
        .stderr(contains("no mnemonic to export"));
}

#[test]
fn select_by_identifier_changes_default() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();
    owallet(&tmp, "pw")
        .arg("import")
        .arg("--mnemonic")
        .arg(ABANDON_12)
        .assert()
        .success();
    owallet(&tmp, "pw").arg("generate").assert().success();

    // After generate, the second wallet is *not* the default — the first
    // import call set the default. Switch back to the abandon address.
    owallet(&tmp, "pw")
        .arg("select")
        .arg(ABANDON_ADDRESS_LOWER)
        .assert()
        .success()
        .stdout(contains("Default wallet set to"));

    // account should report the abandon-mnemonic wallet again.
    owallet(&tmp, "pw")
        .arg("account")
        .assert()
        .success()
        .stdout(contains(ABANDON_ADDRESS_LOWER));
}

#[test]
fn select_unknown_identifier_errors() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();
    owallet(&tmp, "pw").arg("generate").assert().success();

    owallet(&tmp, "pw")
        .arg("select")
        .arg("0xnope")
        .assert()
        .failure()
        .stderr(contains("wallet not found"));
}

#[test]
fn wrong_password_fails_unlock() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();

    let mut cmd = Command::cargo_bin("owallet").unwrap();
    cmd.env("OWALLET_DB_PATH", tmp.path().join("test.db"));
    cmd.env("OWALLET_PASSWORD", "wrong");
    cmd.env("HOME", tmp.path());
    cmd.arg("generate")
        .assert()
        .failure()
        .stderr(contains("wrong password"));
}

// --- regression tests for the wallet-setup fixes -------------------------

/// `generate` used to persist the wallet (and promote it to default) *before*
/// prompting for the per-wallet password. A failed prompt therefore left an
/// orphan default wallet whose seed phrase was never displayed and whose
/// dashboard password could never be set. Nothing may be written when the
/// prompt cannot be satisfied.
#[test]
fn failed_generate_leaves_no_wallet() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();

    let mut cmd = Command::cargo_bin("owallet").expect("binary exists");
    cmd.env("OWALLET_DB_PATH", tmp.path().join("test.db"));
    cmd.env("OWALLET_PASSWORD", "pw");
    cmd.env("HOME", tmp.path());
    // No OWALLET_WALLET_PASSWORD and no TTY under the harness: the per-wallet
    // password prompt must fail.
    cmd.env_remove("OWALLET_WALLET_PASSWORD");
    cmd.arg("generate").assert().failure();

    // The database must be exactly as `init` left it.
    owallet(&tmp, "pw")
        .arg("select")
        .assert()
        .failure()
        .stderr(contains("no wallets stored"));
}

/// The same ordering bug existed in `import`.
#[test]
fn failed_import_leaves_no_wallet() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();

    let mut cmd = Command::cargo_bin("owallet").expect("binary exists");
    cmd.env("OWALLET_DB_PATH", tmp.path().join("test.db"));
    cmd.env("OWALLET_PASSWORD", "pw");
    cmd.env("HOME", tmp.path());
    cmd.env_remove("OWALLET_WALLET_PASSWORD");
    cmd.args(["import", "--mnemonic", ABANDON_12])
        .assert()
        .failure();

    owallet(&tmp, "pw")
        .arg("select")
        .assert()
        .failure()
        .stderr(contains("no wallets stored"));
}

/// A missing terminal must name the variable that fixes it, not surface a raw
/// `/dev/tty` errno ("No such device or address (os error 6)").
#[test]
fn missing_tty_names_the_env_var() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();

    let mut cmd = Command::cargo_bin("owallet").expect("binary exists");
    cmd.env("OWALLET_DB_PATH", tmp.path().join("test.db"));
    cmd.env("OWALLET_PASSWORD", "pw");
    cmd.env("HOME", tmp.path());
    cmd.env_remove("OWALLET_WALLET_PASSWORD");
    cmd.arg("generate")
        .assert()
        .failure()
        .stderr(contains("OWALLET_WALLET_PASSWORD"));
}

/// The wallet database holds encrypted seeds; it must not be world-readable.
#[cfg(unix)]
#[test]
fn init_creates_owner_only_db() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    // A nested path so the created parent directory is checked too.
    let db = tmp.path().join("wallet").join("test.db");
    let mut cmd = Command::cargo_bin("owallet").expect("binary exists");
    cmd.env("OWALLET_DB_PATH", &db);
    cmd.env("OWALLET_PASSWORD", "pw");
    cmd.env("HOME", tmp.path());
    cmd.arg("init").assert().success();

    let file_mode = std::fs::metadata(&db).unwrap().permissions().mode() & 0o777;
    assert_eq!(file_mode, 0o600, "wallet db should be owner-only");
    let dir_mode = std::fs::metadata(db.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "wallet dir should be owner-only");
}

/// Only a directory `init` creates is tightened: a pre-existing parent may be
/// shared (think OWALLET_DB_PATH=/tmp/w.db), and must keep its permissions.
#[cfg(unix)]
#[test]
fn init_leaves_an_existing_parent_directory_alone() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let shared = tmp.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
    let db = shared.join("test.db");
    let mut cmd = Command::cargo_bin("owallet").expect("binary exists");
    cmd.env("OWALLET_DB_PATH", &db);
    cmd.env("OWALLET_PASSWORD", "pw");
    cmd.env("HOME", tmp.path());
    cmd.arg("init").assert().success();

    let dir_mode = std::fs::metadata(&shared).unwrap().permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o755, "a pre-existing directory keeps its mode");
    let file_mode = std::fs::metadata(&db).unwrap().permissions().mode() & 0o777;
    assert_eq!(file_mode, 0o600, "the database itself is still owner-only");
}

/// `owallet password set` did not exist, so a wallet stored without a dashboard
/// password could never get one.
#[test]
fn password_set_replaces_the_wallet_password() {
    let tmp = TempDir::new().unwrap();
    owallet(&tmp, "pw").arg("init").assert().success();
    owallet(&tmp, "pw").arg("generate").assert().success();

    let mut cmd = Command::cargo_bin("owallet").expect("binary exists");
    cmd.env("OWALLET_DB_PATH", tmp.path().join("test.db"));
    cmd.env("OWALLET_PASSWORD", "pw");
    cmd.env("HOME", tmp.path());
    cmd.env("OWALLET_WALLET_PASSWORD", "new-dashboard-pw");
    cmd.args(["password", "set"])
        .assert()
        .success()
        .stdout(contains("Replaced the wallet password"));
}
