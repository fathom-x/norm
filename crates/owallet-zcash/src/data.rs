//! First-run provisioning of the librustzcash wallet data for an account.
//!
//! Creates (or reuses) the encrypted `data.sqlite` + `blockmeta.sqlite`, fetches
//! the wallet birthday from lightwalletd, and creates the single Orchard
//! account. Returns the Orchard Unified Address.

use secrecy::SecretVec;
use zcash_client_backend::{
    data_api::{AccountBirthday, WalletWrite},
    proto::service,
};
use zcash_client_sqlite::{
    chain::init::init_blockmeta_db, wallet::init::init_wallet_db, FsBlockDb,
};
use zcash_protocol::consensus::BlockHeight;

use crate::{
    db::open_wallet_db,
    error::ZcashError,
    keys::orchard_ua_from_seed,
    network::Network,
    paths::{blocks_dir, data_db_path},
    remote,
};

/// Provision the wallet DB + account if not already present, returning the
/// Orchard UA. Idempotent: if an account already exists, returns its UA
/// without touching the chain. `birthday_height` overrides the default
/// (chain tip − 10); pass `None` for fresh wallets.
pub async fn init_account(
    dir: &std::path::Path,
    network: Network,
    lightwalletd: &str,
    seed: &[u8; 64],
    birthday_height: Option<u32>,
) -> Result<String, ZcashError> {
    // Fast path: already initialized.
    if data_db_path(dir).exists() {
        if let Ok(db) = open_wallet_db(dir, network) {
            use zcash_client_backend::data_api::WalletRead;
            if let Ok(ids) = db.get_account_ids() {
                if !ids.is_empty() {
                    return orchard_ua_from_seed(network, seed);
                }
            }
        }
    }

    // Initialize the block cache and the wallet DB.
    let mut db_cache =
        FsBlockDb::for_path(dir).map_err(|e| ZcashError::backend(format!("{e:?}")))?;
    init_blockmeta_db(&mut db_cache).map_err(|e| ZcashError::backend(format!("{e:?}")))?;
    let mut db_data = open_wallet_db(dir, network)?;
    init_wallet_db(&mut db_data, None).map_err(|e| ZcashError::backend(format!("{e:?}")))?;

    // Fetch chain tip + birthday tree state.
    let mut client = remote::connect(lightwalletd, network).await?;
    let chain_tip: u32 = client
        .get_latest_block(service::ChainSpec::default())
        .await
        .map_err(ZcashError::transport)?
        .into_inner()
        .height
        .try_into()
        .map_err(|_| ZcashError::backend("chain tip out of range"))?;

    let birthday_height = birthday_height
        .map(BlockHeight::from)
        .unwrap_or_else(|| BlockHeight::from(chain_tip.saturating_sub(10)));

    let request = service::BlockId {
        height: u64::from(birthday_height).saturating_sub(1),
        ..Default::default()
    };
    let treestate = client
        .get_tree_state(request)
        .await
        .map_err(ZcashError::transport)?
        .into_inner();
    let birthday = AccountBirthday::from_treestate(treestate, Some(chain_tip.into()))
        .map_err(|_| ZcashError::backend("invalid birthday tree state from lightwalletd"))?;

    let secret = SecretVec::new(seed.to_vec());
    db_data
        .create_account("owallet", &secret, &birthday, None)
        .map_err(|e| ZcashError::backend(format!("create account: {e:?}")))?;

    orchard_ua_from_seed(network, seed)
}

/// Path to a compact block file under the cache's `blocks/` directory.
pub(crate) fn block_path(
    dir: &std::path::Path,
    meta: &zcash_client_sqlite::chain::BlockMeta,
) -> std::path::PathBuf {
    meta.block_file_path(&blocks_dir(dir))
}
