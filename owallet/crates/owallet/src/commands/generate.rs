//! `owallet generate` — fresh BIP-39 seed phrase, store + display.

use owallet_crypto::{bip39_seed_from_stored, WordCount};
use owallet_db::{default_db_path, Database, PreparedWallet};

use super::{open_unlock, zcash, CmdError, Result};

/// Derive the Orchard Unified Address from the stored seed (offline) and cache
/// it on the wallet row, returning it for display. Best-effort: a hex-key
/// wallet (no BIP-39 seed) or a bad `ZEC_NETWORK` just skips Zcash silently.
pub(super) fn store_orchard_ua(db: &Database, npub: &str, stored_seed: &str) -> Option<String> {
    let seed = bip39_seed_from_stored(stored_seed).ok()?;
    let network = zcash::network().ok()?;
    let ua = owallet_zcash::orchard_ua_from_seed(network, &seed).ok()?;
    let _ = db.write_zcash_address(npub, &ua);
    Some(ua)
}

pub fn run(words: u8) -> Result<()> {
    let count = match words {
        12 => WordCount::Twelve,
        24 => WordCount::TwentyFour,
        n => {
            return Err(CmdError::BadInput(format!(
                "--words must be 12 or 24, got {n}"
            )))
        }
    };

    let db = open_unlock(&default_db_path())?;
    let wallet = PreparedWallet::generate(count)?;
    let npub = wallet.npub.clone();

    // Collect the per-wallet password (used to log into the web admin) *before
    // anything is persisted*. Deriving the keys above touched only memory, so
    // a failed or abandoned prompt here leaves the database exactly as it was.
    // Persisting first — as this did until now — left a wallet in the DB, made
    // it the default, and never displayed its seed phrase: an orphan wallet the
    // user did not know existed and could not log into, since there is no way
    // to add a wallet password after the fact.
    let wallet_pw = if db.has_wallet_password(&npub)? {
        None
    } else {
        Some(crate::password::read_new_wallet_password()?)
    };

    // Stores the seed, the password, and makes the first wallet the default.
    db.store_wallet(&wallet, wallet_pw.as_ref().map(|pw| pw.as_str()))?;
    let phrase = &wallet.stored_seed;
    // Derive + cache the Orchard receive address (offline). The librustzcash
    // wallet DB itself is created lazily on the first `owallet sync`.
    let zcash_ua = store_orchard_ua(&db, &npub, phrase);
    drop(db); // wipe the in-memory key as soon as possible

    println!("Generated new wallet:");
    println!("  npub:    {npub}");
    println!("  address: {}", wallet.address.to_checksum());
    if let Some(ua) = &zcash_ua {
        println!("  zcash:   {ua}");
    }
    println!();
    println!("WRITE DOWN YOUR SEED PHRASE. It will not be shown again:");
    println!();
    println!("  {phrase}");
    println!();
    println!("Anyone with this phrase can sign as your wallet. It is the only backup.");
    Ok(())
}
