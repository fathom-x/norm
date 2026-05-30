//! `owallet generate` — fresh BIP-39 seed phrase, store + display.

use owallet_crypto::{
    derive_from_mnemonic, npub_from_private_key, Address, Mnemonic, WordCount, EVM_HD_PATH,
};
use owallet_db::default_db_path;

use super::{open_unlock, CmdError, Result};

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
    drop(db); // wipe the in-memory key as soon as possible

    println!("Generated new wallet:");
    println!("  npub:    {npub}");
    println!("  address: {}", address.to_checksum());
    println!();
    println!("WRITE DOWN YOUR SEED PHRASE. It will not be shown again:");
    println!();
    println!("  {phrase}");
    println!();
    println!("Anyone with this phrase can sign as your wallet. It is the only backup.");
    Ok(())
}
