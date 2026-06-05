//! `owallet account` — wallet identity + Overpay account + on-chain balances.
//!
//! Renders one Field/Value table per wallet (the default wallet, or every
//! stored wallet with `--all`). If a Bearer token is stored we use it to fetch
//! the Overpay account; otherwise we fall back to NIP-98 signing via the wallet
//! key. EVM balances are fetched live (best-effort). Zcash shows the Orchard
//! receive address and the *local* balance — run `owallet sync` to refresh it
//! from the network.

use owallet_crypto::{bip39_seed_from_stored, derive_from_stored_seed, Address};
use owallet_db::{default_db_path, Database, WalletRow};
use owallet_overpay::Auth;

use super::overpay::{block_on, client as overpay_client, host_key};
use super::{open_unlock, zcash, CmdError, Result};

pub fn run(all: bool) -> Result<()> {
    let db = open_unlock(&default_db_path())?;
    let wallets = db.list_wallets()?;

    if wallets.is_empty() {
        println!("No wallets stored. Run `owallet generate` or `owallet import`.");
        return Ok(());
    }

    let npubs: Vec<String> = if all {
        wallets.iter().map(|w| w.npub.clone()).collect()
    } else {
        let npub = db
            .read_default_npub()?
            .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?;
        vec![npub]
    };

    let default_npub = db.read_default_npub()?;

    let total = npubs.len();
    for (i, npub) in npubs.iter().enumerate() {
        let w = wallets
            .iter()
            .find(|w| &w.npub == npub)
            .ok_or_else(|| CmdError::NotFound(npub.clone()))?;
        if all {
            let marker = if Some(npub) == default_npub.as_ref() {
                " (default)"
            } else {
                ""
            };
            println!("Wallet {}{}", i + 1, marker);
        }
        print_wallet_table(&db, w)?;
        if all && i + 1 < total {
            println!();
            println!("{}", "─".repeat(90));
            println!();
            // Avoid hitting the free public RPC rate limit when looping over many wallets.
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    Ok(())
}

fn print_wallet_table(db: &Database, w: &WalletRow) -> Result<()> {
    let npub = &w.npub;

    // Resolve address: stored or derived from seed.
    let derived_addr;
    let address = if let Some(a) = w.address.as_deref() {
        a
    } else if let Some(seed) = db.read_seed(npub)? {
        let sk = derive_from_stored_seed(&seed)?;
        derived_addr = Address::from_private_key(&sk).to_hex_lower();
        let _ = db.cache_wallet_address(npub, &derived_addr);
        &derived_addr
    } else {
        ""
    };

    // EVM config.
    let rpc_url =
        std::env::var("EVM_RPC_URL").unwrap_or_else(|_| "https://mainnet.base.org".into());
    let network = std::env::var("EVM_NETWORK").unwrap_or_else(|_| "eip155:8453".into());

    // Fetch Overpay account info + merchant credits (best-effort).
    let stored_token = db.read_token(npub, &host_key())?;
    let seed = db.read_seed(npub)?;
    let (overpay_info, credits_list) = if let Ok(client) = overpay_client() {
        if let Some(t) = stored_token.as_deref() {
            let info = block_on(async { client.account(Auth::Bearer(t)).await }).ok();
            let credits =
                block_on(async { client.list_merchant_credits(Auth::Bearer(t)).await }).ok();
            (info, credits)
        } else if let Some(s) = seed.as_deref() {
            let maybe_sk = derive_from_stored_seed(s).ok();
            let info = maybe_sk
                .as_ref()
                .and_then(|sk| block_on(async { client.account(Auth::Nip98(sk)).await }).ok());
            let credits = maybe_sk.as_ref().and_then(|sk| {
                block_on(async { client.list_merchant_credits(Auth::Nip98(sk)).await }).ok()
            });
            (info, credits)
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };

    if let Some(info) = &overpay_info {
        if let Some(u) = info.username.as_deref() {
            let _ = db.cache_wallet_username(npub, u);
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
                            format!(
                                "{} USDC",
                                owallet_evm::format_amount(v, chain.usdc_decimals)
                            )
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

    // Zcash: Orchard receive address (stored or derived offline) + the local
    // balance. No network here — `owallet sync` refreshes from lightwalletd.
    let zcash_addr = zcash_address_str(db, npub, w.zcash_address.as_deref(), seed.as_deref());
    let zec_str = zec_balance_str(npub, seed.as_deref());

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

    let mut rows: Vec<(String, String)> = vec![
        ("Address".into(), address.to_string()),
        ("Network".into(), network.clone()),
        ("npub".into(), npub.to_string()),
        ("ETH Balance".into(), eth_str),
        ("USDC Balance".into(), usdc_str),
        ("Zcash Address".into(), zcash_addr),
        ("ZEC Balance".into(), zec_str),
        ("Username".into(), username),
        ("Account Number".into(), account_number.to_string()),
    ];

    if let Some(credits) = &credits_list {
        for c in &credits.data {
            let slug = c
                .organization_slug
                .as_deref()
                .or(c.seller_slug.as_deref())
                .unwrap_or("?");
            let label = format!("Credits ({slug})");
            let balance = c.formatted_balance.as_deref().unwrap_or("?").to_string();
            rows.push((label, balance));
        }
    }

    println!("{:<14}  Value", "Field");
    println!("{:-<14}  {:-<72}", "", "");
    for (k, v) in &rows {
        println!("{:<14}  {}", k, v);
    }

    Ok(())
}

/// The wallet's Orchard Unified Address: the stored value, or derived offline
/// from the seed (and cached). `—` for hex-key wallets (no Zcash account).
fn zcash_address_str(
    db: &Database,
    npub: &str,
    stored: Option<&str>,
    seed: Option<&str>,
) -> String {
    if let Some(z) = stored {
        return z.to_string();
    }
    let Some(s) = seed else {
        return "—".into();
    };
    let (Ok(bseed), Ok(network)) = (bip39_seed_from_stored(s), zcash::network()) else {
        return "—".into();
    };
    match owallet_zcash::orchard_ua_from_seed(network, &bseed) {
        Ok(ua) => {
            let _ = db.write_zcash_address(npub, &ua);
            ua
        }
        Err(_) => "—".into(),
    }
}

/// Local (no-network) ZEC balance. `owallet sync` refreshes from lightwalletd;
/// an unsynced wallet reads as zero. `—` for hex-key wallets.
fn zec_balance_str(npub: &str, seed: Option<&str>) -> String {
    let Some(s) = seed else {
        return "—".into();
    };
    if bip39_seed_from_stored(s).is_err() {
        return "—".into();
    }
    let (Ok(network), Ok(dir)) = (zcash::network(), zcash::data_dir(npub)) else {
        return "—".into();
    };
    match owallet_zcash::zec_balance(&dir, network) {
        Ok(b) => format!(
            "{} {} (spendable {}; run `owallet sync` to refresh)",
            owallet_zcash::format_zec(b.total_zat),
            network.ticker(),
            owallet_zcash::format_zec(b.spendable_zat),
        ),
        Err(e) => format!("(error: {e})"),
    }
}
