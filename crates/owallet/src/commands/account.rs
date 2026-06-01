//! `owallet account` — wallet identity + Overpay account + on-chain balances.
//!
//! If a Bearer token is stored we use it to fetch the Overpay account.
//! Otherwise we fall back to NIP-98 signing via the wallet key.

use owallet_crypto::{derive_from_stored_seed, Address};
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

    let npub = db
        .read_default_npub()?
        .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?;
    let w = wallets
        .iter()
        .find(|w| w.npub == npub)
        .ok_or_else(|| CmdError::NotFound(npub.clone()))?;

    // Resolve address: stored or derived from seed.
    let derived_addr;
    let address = if let Some(a) = w.address.as_deref() {
        a
    } else if let Some(seed) = db.read_seed(&npub)? {
        let sk = derive_from_stored_seed(&seed)?;
        derived_addr = Address::from_private_key(&sk).to_hex_lower();
        let _ = db.cache_wallet_address(&npub, &derived_addr);
        &derived_addr
    } else {
        ""
    };

    // EVM config.
    let rpc_url =
        std::env::var("EVM_RPC_URL").unwrap_or_else(|_| "https://mainnet.base.org".into());
    let network = std::env::var("EVM_NETWORK").unwrap_or_else(|_| "eip155:8453".into());

    // Fetch Overpay account info (best-effort).
    let stored_token = db.read_token(&npub, &host_key())?;
    let seed = db.read_seed(&npub)?;
    let overpay_info = if let Ok(client) = overpay_client() {
        if let Some(t) = stored_token.as_deref() {
            block_on(async { client.account(Auth::Bearer(t)).await }).ok()
        } else if let Some(s) = seed.as_deref() {
            derive_from_stored_seed(s)
                .ok()
                .and_then(|sk| block_on(async { client.account(Auth::Nip98(&sk)).await }).ok())
        } else {
            None
        }
    } else {
        None
    };

    if let Some(info) = &overpay_info {
        if let Some(u) = info.username.as_deref() {
            let _ = db.cache_wallet_username(&npub, u);
        }
    }

    // Fetch on-chain balances (best-effort).
    let rails_url =
        std::env::var("OVERPAY_RAILS_URL").unwrap_or_else(|_| "https://overpay.com".into());
    let (eth_str, usdc_str) = if !address.is_empty() {
        match owallet_evm::chains::from_caip2(&network) {
            Ok(chain) => {
                let eth = block_on(async { owallet_evm::eth_balance(&rpc_url, address).await })
                    .map(|v| format!("{} ETH", owallet_evm::format_amount(v, 18)))
                    .unwrap_or_else(|e| format!("(error: {e})"));
                let usdc =
                    block_on(async { owallet_evm::usdc_balance(&rpc_url, &chain, address).await })
                        .map(|v| {
                            format!("{} USDC", owallet_evm::format_amount(v, chain.usdc_decimals))
                        })
                        .unwrap_or_else(|e| format!("(error: {e})"));
                (eth, usdc)
            }
            Err(e) => (
                format!("(bad EVM_NETWORK: {e})"),
                format!("(bad EVM_NETWORK: {e})"),
            ),
        }
    } else {
        ("(no address)".into(), "(no address)".into())
    };

    let username = overpay_info
        .as_ref()
        .and_then(|i| i.username.as_deref())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!("No Overpay account linked to this wallet. Sign up at {rails_url}")
        });
    let account_number = overpay_info
        .as_ref()
        .and_then(|i| i.account_number.as_deref())
        .unwrap_or("—");

    let rows: &[(&str, &str)] = &[
        ("Address", address),
        ("Network", &network),
        ("npub", &npub),
        ("ETH Balance", &eth_str),
        ("USDC Balance", &usdc_str),
        ("Username", &username),
        ("Account Number", account_number),
    ];

    println!("{:<14}  Value", "Field");
    println!("{:-<14}  {:-<72}", "", "");
    for (k, v) in rows {
        println!("{:<14}  {}", k, v);
    }

    Ok(())
}
