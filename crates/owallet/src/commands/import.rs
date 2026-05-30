//! `owallet import` — bring an existing BIP-39 mnemonic or hex private key
//! into the encrypted DB.

use owallet_crypto::{
    derive_from_mnemonic, npub_from_private_key, Address, Mnemonic, PrivateKey, EVM_HD_PATH,
};
use owallet_db::default_db_path;

use super::{open_unlock, Result};

pub fn run(mnemonic: Option<String>, private_key: Option<String>) -> Result<()> {
    let db = open_unlock(&default_db_path())?;

    let (stored_seed, sk) = match (mnemonic, private_key) {
        (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        (Some(phrase), None) => {
            let m = Mnemonic::parse(&phrase)?;
            let sk = derive_from_mnemonic(&m, EVM_HD_PATH)?;
            (m.phrase(), sk)
        }
        (None, Some(hex)) => {
            let sk = PrivateKey::from_hex(&hex)?;
            (format!("0x{}", sk.to_hex()), sk)
        }
        (None, None) => {
            let typed =
                rpassword::prompt_password("Mnemonic phrase or hex private key (input hidden): ")?;
            let trimmed = typed.trim();
            if trimmed.split_whitespace().count() >= 12 {
                let m = Mnemonic::parse(trimmed)?;
                let sk = derive_from_mnemonic(&m, EVM_HD_PATH)?;
                (m.phrase(), sk)
            } else {
                let sk = PrivateKey::from_hex(trimmed)?;
                (format!("0x{}", sk.to_hex()), sk)
            }
        }
    };

    let address = Address::from_private_key(&sk);
    let npub = npub_from_private_key(&sk)?;

    db.write_wallet(&npub, &stored_seed, Some(&address.to_hex_lower()))?;
    if db.read_default_npub()?.is_none() {
        db.write_default_npub(&npub)?;
    }
    // Set a per-wallet password (used to log into the web admin) unless one
    // already exists — matches `import` in wallet_mcp/cli.py.
    if !db.has_wallet_password(&npub)? {
        let wallet_pw = crate::password::read_new_wallet_password()?;
        db.write_wallet_password(&npub, wallet_pw.as_str())?;
    }

    println!("Imported wallet:");
    println!("  npub:    {npub}");
    println!("  address: {}", address.to_checksum());
    Ok(())
}
