//! `owallet send --to ADDRESS --amount USDC` — sign + broadcast an ERC-20
//! USDC transfer on the configured EVM chain.

use owallet_crypto::derive_from_stored_seed;
use owallet_db::default_db_path;
use owallet_evm::chains;

use super::overpay::block_on;
use super::{open_unlock, CmdError, Result};

pub fn run(to: &str, amount_usd: f64) -> Result<()> {
    let db = open_unlock(&default_db_path())?;
    let npub = db
        .read_default_npub()?
        .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?;
    let seed = db
        .read_seed(&npub)?
        .ok_or_else(|| CmdError::NotFound(npub.clone()))?;
    let sk = derive_from_stored_seed(&seed)?;

    let rpc_url =
        std::env::var("EVM_RPC_URL").unwrap_or_else(|_| "https://mainnet.base.org".into());
    let network = std::env::var("EVM_NETWORK").unwrap_or_else(|_| "eip155:8453".into());
    let chain = chains::from_caip2(&network)?;

    println!(
        "Sending {amount_usd} USDC on {} ({}) to {to} …",
        chain.name, network
    );

    let outcome =
        block_on(async { owallet_evm::send_usdc(&rpc_url, &chain, &sk, to, amount_usd).await })?;

    println!();
    println!("Broadcast successful:");
    println!("  tx hash:  {}", outcome.tx_hash);
    println!("  to:       {}", outcome.to);
    println!(
        "  amount:   {} USDC ({} raw)",
        outcome.amount_human, outcome.amount_raw
    );
    if let Some(b) = outcome.block_number {
        println!("  block:    {b}");
    }
    if let Some(url) = outcome.explorer_url.as_deref() {
        println!("  explorer: {url}");
    }
    Ok(())
}
