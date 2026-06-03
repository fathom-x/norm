//! Wallet balance, read from the encrypted `data.sqlite`. No network access —
//! call [`crate::sync::sync`] first to bring the wallet up to the chain tip.

use zcash_client_backend::data_api::{wallet::ConfirmationsPolicy, WalletRead};

use crate::{db::open_wallet_db, error::ZcashError, network::Network, paths::data_db_path};

/// Zatoshi balances for the wallet's account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZecBalance {
    /// Everything the wallet has detected, confirmed or not.
    pub total_zat: u64,
    /// Confirmed Orchard notes that can be spent right now.
    pub spendable_zat: u64,
    /// Incoming/change value not yet spendable (awaiting confirmations).
    pub pending_zat: u64,
}

/// Compute the wallet's balance from local state. Returns all-zero if the
/// wallet has never been synced (no summary yet).
pub fn zec_balance(dir: &std::path::Path, network: Network) -> Result<ZecBalance, ZcashError> {
    // Never synced: no wallet DB on disk yet. Report zero rather than
    // creating an empty file or erroring.
    if !data_db_path(dir).exists() {
        return Ok(ZecBalance::default());
    }
    let db = open_wallet_db(dir, network)?;
    let Some(summary) = db
        .get_wallet_summary(ConfirmationsPolicy::default())
        .map_err(|e| ZcashError::backend(format!("{e:?}")))?
    else {
        return Ok(ZecBalance::default());
    };

    let mut out = ZecBalance::default();
    for b in summary.account_balances().values() {
        out.total_zat += u64::from(b.total());
        let orchard = b.orchard_balance();
        out.spendable_zat += u64::from(orchard.spendable_value());
        out.pending_zat += u64::from(orchard.value_pending_spendability())
            + u64::from(orchard.change_pending_confirmation());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcash_client_sqlite::wallet::init::init_wallet_db;

    // No network: exercises the DB open + the "never synced / no summary yet"
    // zero paths.
    #[test]
    fn balance_is_zero_before_and_after_empty_init() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // No data.sqlite on disk yet -> zero, and no file is created.
        assert_eq!(
            zec_balance(dir, Network::Main).unwrap(),
            ZecBalance::default()
        );
        assert!(!crate::paths::data_db_path(dir).exists());

        // Initialize an empty (account-less) wallet DB -> still zero
        // (get_wallet_summary returns None).
        {
            let mut db = crate::db::open_wallet_db(dir, Network::Main).unwrap();
            init_wallet_db(&mut db, None).unwrap();
        }
        assert!(crate::paths::data_db_path(dir).exists());
        assert_eq!(
            zec_balance(dir, Network::Main).unwrap(),
            ZecBalance::default()
        );
    }
}
