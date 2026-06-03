//! `owallet import` — bring an existing BIP-39 mnemonic or hex private key
//! into the encrypted DB.

use owallet_crypto::{
    bip39_seed_from_stored, derive_from_mnemonic, npub_from_private_key, Address, Mnemonic,
    PrivateKey, EVM_HD_PATH,
};
use owallet_db::default_db_path;

use super::generate::store_orchard_ua;
use super::overpay::block_on;
use super::{open_unlock, zcash, Result};

pub fn run(
    mnemonic: Option<String>,
    private_key: Option<String>,
    zec_birthday: Option<u32>,
) -> Result<()> {
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
    // Cache the Orchard receive address (offline).
    let zcash_ua = store_orchard_ua(&db, &npub, &stored_seed);

    println!("Imported wallet:");
    println!("  npub:    {npub}");
    println!("  address: {}", address.to_checksum());
    if let Some(ua) = &zcash_ua {
        println!("  zcash:   {ua}");
    }

    // If the user supplied a birthday, provision the librustzcash wallet DB
    // now so the first sync scans from that height (recovering older Orchard
    // funds). Requires network; a failure is non-fatal — the address is
    // already saved and a later `owallet sync` will provision from the tip.
    if let Some(height) = zec_birthday {
        if let Ok(seed) = bip39_seed_from_stored(&stored_seed) {
            match (zcash::network(), zcash::data_dir(&npub)) {
                (Ok(network), Ok(dir)) => {
                    let lwd = zcash::lightwalletd();
                    match block_on(async {
                        owallet_zcash::init_account(&dir, network, &lwd, &seed, Some(height)).await
                    }) {
                        Ok(_) => println!("  zcash birthday set to height {height}"),
                        Err(e) => eprintln!("(could not set Zcash birthday now: {e})"),
                    }
                }
                _ => eprintln!("(skipping Zcash birthday: bad ZEC_NETWORK/ZEC_DATA_DIR)"),
            }
        }
    }
    Ok(())
}
