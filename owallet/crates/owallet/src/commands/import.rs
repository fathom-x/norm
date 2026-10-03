//! `owallet import` — bring an existing BIP-39 mnemonic or hex private key
//! into the encrypted DB.

use owallet_crypto::bip39_seed_from_stored;
use owallet_db::{default_db_path, PreparedWallet};

use super::generate::store_orchard_ua;
use super::overpay::block_on;
use super::{open_unlock, zcash, Result};

pub fn run(
    mnemonic: Option<String>,
    private_key: Option<String>,
    zec_birthday: Option<u32>,
) -> Result<()> {
    let db = open_unlock(&default_db_path())?;

    let wallet = match (mnemonic, private_key) {
        (Some(_), Some(_)) => unreachable!("clap enforces conflicts_with"),
        (Some(phrase), None) => PreparedWallet::from_phrase(&phrase)?,
        (None, Some(hex)) => PreparedWallet::from_private_key_hex(&hex)?,
        (None, None) => {
            let typed =
                rpassword::prompt_password("Mnemonic phrase or hex private key (input hidden): ")?;
            PreparedWallet::from_secret(&typed)?
        }
    };
    let npub = wallet.npub.clone();
    let stored_seed = wallet.stored_seed.clone();

    // Collect the per-wallet password before persisting anything — see the
    // matching comment in `generate`: a failed prompt used to leave an orphan
    // wallet in the DB, already promoted to default.
    let wallet_pw = if db.has_wallet_password(&npub)? {
        None
    } else {
        Some(crate::password::read_new_wallet_password()?)
    };

    db.store_wallet(&wallet, wallet_pw.as_ref().map(|pw| pw.as_str()))?;
    // Cache the Orchard receive address (offline).
    let zcash_ua = store_orchard_ua(&db, &npub, &stored_seed);

    println!("Imported wallet:");
    println!("  npub:    {npub}");
    println!("  address: {}", wallet.address.to_checksum());
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
