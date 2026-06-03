//! `owallet sync` — bring the default wallet's Zcash (Orchard) state up to the
//! chain tip via lightwalletd and print the synced height + balance.

use owallet_crypto::bip39_seed_from_stored;
use owallet_db::default_db_path;

use super::overpay::block_on;
use super::{open_unlock, zcash, CmdError, Result};

pub fn run() -> Result<()> {
    let db = open_unlock(&default_db_path())?;
    let npub = db
        .read_default_npub()?
        .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?;
    let stored = db
        .read_seed(&npub)?
        .ok_or_else(|| CmdError::NotFound(npub.clone()))?;
    let seed = bip39_seed_from_stored(&stored)?;

    let network = zcash::network()?;
    let lwd = zcash::lightwalletd();
    let dir = zcash::data_dir(&npub)?;

    // Ensure the wallet DB + account exist (idempotent), then sync.
    println!("Syncing Zcash ({}) …", network.name());
    block_on(async { owallet_zcash::init_account(&dir, network, &lwd, &seed, None).await })?;
    let height = block_on(async { owallet_zcash::sync(&dir, network, &lwd).await })?;
    let balance = owallet_zcash::zec_balance(&dir, network)?;

    println!("  synced height: {height}");
    println!(
        "  balance:       {} {} (spendable {} {})",
        owallet_zcash::format_zec(balance.total_zat),
        network.ticker(),
        owallet_zcash::format_zec(balance.spendable_zat),
        network.ticker(),
    );
    Ok(())
}
