//! `owallet sync` — fetch live balances for all supported assets.
//! EVM (ETH + USDC) runs for every wallet type; Zcash sync runs only for
//! mnemonic-backed wallets (hex-key wallets have no BIP-39 seed).

use owallet_crypto::{bip39_seed_from_stored, derive_from_stored_seed, Address};
use owallet_db::default_db_path;
use owallet_evm::chains;

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

    // EVM balances — works for hex-key and mnemonic wallets alike.
    let sk = derive_from_stored_seed(&stored)?;
    let evm_address = Address::from_private_key(&sk).to_checksum();
    let rpc_url =
        std::env::var("EVM_RPC_URL").unwrap_or_else(|_| "https://mainnet.base.org".into());
    let network = std::env::var("EVM_NETWORK").unwrap_or_else(|_| "eip155:8453".into());
    let chain = chains::from_caip2(&network)?;

    println!("EVM ({} — {evm_address}):", chain.name);
    match block_on(async { owallet_evm::usdc_balance(&rpc_url, &chain, &evm_address).await }) {
        Ok(raw) => println!("  USDC: {}", owallet_evm::format_amount(raw, 6)),
        Err(e) => println!("  USDC: (error: {e})"),
    }
    match block_on(async { owallet_evm::eth_balance(&rpc_url, &evm_address).await }) {
        Ok(raw) => println!("  ETH:  {}", owallet_evm::format_amount(raw, 18)),
        Err(e) => println!("  ETH:  (error: {e})"),
    }

    // Zcash sync — only for mnemonic-backed wallets.
    match bip39_seed_from_stored(&stored) {
        Ok(seed) => {
            let zec_network = zcash::network()?;
            let lwd = zcash::lightwalletd();
            let dir = zcash::data_dir(&npub)?;

            println!("Zcash ({}) …", zec_network.name());
            block_on(async {
                owallet_zcash::init_account(&dir, zec_network, &lwd, &seed, None).await
            })?;
            let height = block_on(async { owallet_zcash::sync(&dir, zec_network, &lwd).await })?;
            let balance = owallet_zcash::zec_balance(&dir, zec_network)?;

            println!("  synced height: {height}");
            println!(
                "  balance:       {} {} (spendable {})",
                owallet_zcash::format_zec(balance.total_zat),
                zec_network.ticker(),
                owallet_zcash::format_zec(balance.spendable_zat),
            );
        }
        Err(_) => {
            println!("Zcash: not available (hex-key wallet — import a mnemonic to enable)");
        }
    }

    Ok(())
}
