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
