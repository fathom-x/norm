//! `owallet account` — wallet metadata + the linked Overpay account.
//!
//! If a Bearer token is stored we use it. Otherwise we fall back to a
//! NIP-98-signed request using the wallet key — many Overpay endpoints
//! accept the wallet's npub identity directly.

use owallet_crypto::derive_from_stored_seed;
use owallet_db::default_db_path;
use owallet_overpay::Auth;

use super::overpay::{block_on, client as overpay_client, host_key};
use super::{open_unlock, CmdError, Result};

pub fn run() -> Result<()> {
    let db = open_unlock(&default_db_path())?;
    let wallets = db.list_wallets()?;

    if wallets.is_empty() {
        println!("No wallets stored. Run `owallet generate` or `owallet import`.");
        return Ok(());
    }

    let chosen = db
        .read_default_npub()?
        .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?;
    let w = wallets
        .iter()
        .find(|w| w.npub == chosen)
        .ok_or_else(|| CmdError::NotFound(chosen.clone()))?;

    println!("Default wallet:");
    println!("  npub:    {}", w.npub);
    if let Some(a) = &w.address {
        println!("  address: {a}");
    }
    if let Some(u) = &w.overpay_username {
        println!("  overpay: {u}");
    }
    if let Some(ts) = w.last_accessed {
        println!("  last accessed (unix): {ts}");
    }

    let stored_token = db.read_token(&w.npub, &host_key())?;
    let seed = db.read_seed(&w.npub)?;

    let overpay = overpay_client()?;
    let (auth_label, fetch) = if let Some(t) = stored_token.as_deref() {
        (
            "Bearer",
            block_on(async { overpay.account(Auth::Bearer(t)).await }),
        )
    } else if let Some(s) = seed.as_deref() {
        match derive_from_stored_seed(s) {
            Ok(sk) => (
                "NIP-98 (no Bearer stored — run `owallet authorize` to skip this signing)",
                block_on(async { overpay.account(Auth::Nip98(&sk)).await }),
            ),
            Err(e) => {
                eprintln!("(could not derive wallet key for NIP-98: {e})");
                return Ok(());
            }
        }
    } else {
        return Ok(());
    };

    match fetch {
        Ok(info) => {
            println!();
            println!("Linked Overpay account ({auth_label}):");
            if let Some(u) = info.username.as_deref() {
                println!("  username:       {u}");
                let _ = db.cache_wallet_username(&w.npub, u);
            }
            if let Some(n) = info.account_number.as_deref() {
                println!("  account number: {n}");
            }
            if let Some(e) = info.email.as_deref() {
                println!("  email:          {e}");
            }
        }
        Err(e) => {
            eprintln!("(could not refresh Overpay account info: {e})");
        }
    }

    // On-chain balances. Best-effort — RPC failure prints a notice but
    // never panics or short-circuits the rest of the command.
    if let Some(addr) = w.address.as_deref() {
        print_onchain_balances(addr);
    }

    Ok(())
}

fn print_onchain_balances(address: &str) {
    let rpc_url =
        std::env::var("EVM_RPC_URL").unwrap_or_else(|_| "https://mainnet.base.org".into());
    let network = std::env::var("EVM_NETWORK").unwrap_or_else(|_| "eip155:8453".into());
    let chain = match owallet_evm::chains::from_caip2(&network) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("(skipping on-chain balances: bad EVM_NETWORK {network:?}: {e})");
            return;
        }
    };

    println!();
    println!("On-chain balances on {} ({}):", chain.name, network);
    match block_on(async { owallet_evm::eth_balance(&rpc_url, address).await }) {
        Ok(v) => println!("  eth:  {} ETH", owallet_evm::format_amount(v, 18)),
        Err(e) => println!("  eth:  (could not fetch: {e})"),
    }
    match block_on(async { owallet_evm::usdc_balance(&rpc_url, &chain, address).await }) {
        Ok(v) => println!(
            "  usdc: {} USDC",
            owallet_evm::format_amount(v, chain.usdc_decimals)
        ),
        Err(e) => println!("  usdc: (could not fetch: {e})"),
    }
}
