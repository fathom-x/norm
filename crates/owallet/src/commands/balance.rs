//! `owallet balance` — print on-chain ETH + USDC balances for the active wallet.

use owallet_crypto::{derive_from_stored_seed, Address};
use owallet_db::default_db_path;

use super::overpay::block_on;
use super::{open_unlock, CmdError, Result};

pub fn run() -> Result<()> {
    let db = open_unlock(&default_db_path())?;

    let npub = db
        .read_default_npub()?
        .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?;
    let wallets = db.list_wallets()?;
    let w = wallets
        .iter()
        .find(|w| w.npub == npub)
        .ok_or_else(|| CmdError::NotFound(npub.clone()))?;

    // Use the stored address if present; otherwise derive it from the seed
    // (wallets migrated from the Python DB may not have an address stored).
    let derived;
    let address = if let Some(a) = w.address.as_deref() {
        a
    } else {
        let seed = db.read_seed(&npub)?.ok_or_else(|| {
            CmdError::BadInput("no EVM address or seed stored — run `owallet import`".into())
        })?;
        let sk = derive_from_stored_seed(&seed)?;
        derived = Address::from_private_key(&sk).to_hex_lower();
        let _ = db.cache_wallet_address(&npub, &derived);
        &derived
    };

    let rpc_url =
        std::env::var("EVM_RPC_URL").unwrap_or_else(|_| "https://mainnet.base.org".into());
    let network = std::env::var("EVM_NETWORK").unwrap_or_else(|_| "eip155:8453".into());
    let chain = owallet_evm::chains::from_caip2(&network)
        .map_err(|e| CmdError::BadInput(format!("bad EVM_NETWORK {network:?}: {e}")))?;

    println!("Address: {address}");
    println!("Network: {} ({})", chain.name, network);
    println!();

    match block_on(async { owallet_evm::eth_balance(&rpc_url, address).await }) {
        Ok(v) => println!("  ETH:  {}", owallet_evm::format_amount(v, 18)),
        Err(e) => println!("  ETH:  (could not fetch: {e})"),
    }
    match block_on(async { owallet_evm::usdc_balance(&rpc_url, &chain, address).await }) {
        Ok(v) => println!(
            "  USDC: {}",
            owallet_evm::format_amount(v, chain.usdc_decimals)
        ),
        Err(e) => println!("  USDC: (could not fetch: {e})"),
    }

    Ok(())
}
