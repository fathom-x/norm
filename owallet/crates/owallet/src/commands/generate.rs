//! `owallet generate` — fresh BIP-39 seed phrase, store + display.

use owallet_crypto::{
    bip39_seed_from_stored, derive_from_mnemonic, npub_from_private_key, Address, Mnemonic,
    WordCount, EVM_HD_PATH,
};
use owallet_db::{default_db_path, Database};

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
    let mnemonic = Mnemonic::generate(count);
    let phrase = mnemonic.phrase();
    let sk = derive_from_mnemonic(&mnemonic, EVM_HD_PATH)?;
    let address = Address::from_private_key(&sk);
    let npub = npub_from_private_key(&sk)?;

    db.write_wallet(&npub, &phrase, Some(&address.to_hex_lower()))?;
    // First wallet becomes the default automatically.
    if db.read_default_npub()?.is_none() {
        db.write_default_npub(&npub)?;
    }
    // Set a per-wallet password (used to log into the web admin) unless one
    // already exists — matches `generate` in wallet_mcp/cli.py.
    if !db.has_wallet_password(&npub)? {
        let wallet_pw = crate::password::read_new_wallet_password()?;
        db.write_wallet_password(&npub, wallet_pw.as_str())?;
    }
    // Derive + cache the Orchard receive address (offline). The librustzcash
    // wallet DB itself is created lazily on the first `owallet sync`.
    let zcash_ua = store_orchard_ua(&db, &npub, &phrase);
    drop(db); // wipe the in-memory key as soon as possible

    println!("Generated new wallet:");
    println!("  npub:    {npub}");
    println!("  address: {}", address.to_checksum());
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
