//! `owallet password set` — set or replace a wallet's web-admin password.
//!
//! Without this there was no way to add a wallet password after the fact: a
//! wallet stored without one (older databases, or a wallet left behind by a
//! `generate` that failed at its password prompt) could never log into the
//! dashboard, and nothing in the CLI could repair it.
//!
//! This is the *wallet* password (web admin), not the database password that
//! encrypts everything at rest — changing that one would mean re-encrypting
//! every stored secret, which `init` owns.

use owallet_db::default_db_path;

use super::{open_unlock, CmdError, Result};

pub fn run(npub_arg: Option<String>) -> Result<()> {
    let db = open_unlock(&default_db_path())?;

    let npub = match npub_arg {
        Some(n) => n,
        None => db
            .read_default_npub()?
            .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?,
    };

    // Confirm the wallet exists before prompting, so a typo'd npub fails fast
    // instead of after two password entries.
    if db.read_seed(&npub)?.is_none() {
        return Err(CmdError::NotFound(npub));
    }

    let existed = db.has_wallet_password(&npub)?;
    let pw = crate::password::read_new_wallet_password()?;
    db.write_wallet_password(&npub, pw.as_str())?;

    println!(
        "{} the wallet password for {npub}.",
        if existed { "Replaced" } else { "Set" }
    );
    println!("Use it to log into the owallet dashboard.");
    Ok(())
}
