//! `owallet send --to ADDRESS --amount N [--asset usdc|zec]` — sign + broadcast
//! a payment. USDC goes out as an ERC-20 transfer on the configured EVM chain;
//! ZEC goes out as a shielded Orchard payment to a Unified Address.

use owallet_crypto::{bip39_seed_from_stored, derive_from_stored_seed};
use owallet_db::default_db_path;
use owallet_evm::chains;

use crate::cli::Asset;

use super::overpay::block_on;
use super::{open_unlock, zcash, CmdError, Result};

pub fn run(to: &str, amount: f64, asset: Asset) -> Result<()> {
    match asset {
        Asset::Usdc => send_usdc(to, amount),
        Asset::Zec => send_zec(to, amount),
    }
}

fn send_usdc(to: &str, amount_usd: f64) -> Result<()> {
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

fn send_zec(to: &str, amount_zec: f64) -> Result<()> {
    let db = open_unlock(&default_db_path())?;
    let npub = db
        .read_default_npub()?
        .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?;
    let stored = db
        .read_seed(&npub)?
        .ok_or_else(|| CmdError::NotFound(npub.clone()))?;
    let seed = bip39_seed_from_stored(&stored)?;

    let network = zcash::network()?;
    let lwd = zcash::lightwalletd();
    let dir = zcash::data_dir(&npub)?;

    println!(
        "Syncing Zcash wallet on {} before sending …",
        network.name()
    );
    block_on(async { owallet_zcash::sync(&dir, network, &lwd).await })?;

    println!("Sending {amount_zec} ZEC to {to} …");
    let outcome = block_on(async {
        owallet_zcash::send_zcash(&dir, network, &lwd, &seed, to, amount_zec).await
    })?;

    println!();
    println!("Broadcast successful:");
    println!("  txid:   {}", outcome.txid);
    println!("  to:     {}", outcome.to);
    println!(
        "  amount: {} ZEC ({} zat)",
        outcome.amount_human, outcome.amount_zat
    );
    for extra in &outcome.other_txids {
        println!("  (additional tx: {extra})");
    }
    Ok(())
}
