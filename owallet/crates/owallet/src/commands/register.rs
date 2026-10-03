//! `owallet register` — zero-click Overpay sign-up for the default wallet.
//!
//! NIP-98-signs `POST /api/v1/buyer/register` with the wallet's own key:
//! Overpay finds or creates the account bound to that key and returns an API
//! token, stored exactly where `owallet authorize` stores its OAuth bearer
//! (same host key), so every other command and `owallet serve` see the
//! wallet as linked. The browser build's `/_mgmt/overpay/register` is the
//! same flow.

use owallet_crypto::derive_from_stored_seed;
use owallet_db::default_db_path;
use owallet_overpay::Auth;
use serde_json::json;

use super::overpay::{block_on, client as overpay_client, host_key};
use super::{open_unlock, CmdError, Result};

pub fn run(json: bool) -> Result<()> {
    let db = open_unlock(&default_db_path())?;
    let npub = db
        .read_default_npub()?
        .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?;
    let seed = db
        .read_seed(&npub)?
        .ok_or_else(|| CmdError::NotFound(npub.clone()))?;
    let sk = derive_from_stored_seed(&seed)?;
    drop(seed);

    let overpay = overpay_client()?;
    let host = host_key();
    block_on(async move {
        let reg = overpay.register_buyer(&sk, Some("owallet-cli")).await?;
        drop(sk);
        db.write_token(&npub, &host, &reg.token, "overpay-nip98")?;
        // Best effort, like `authorize`: confirm the link, cache the name.
        let account = overpay.account(Auth::Bearer(&reg.token)).await.ok();
        let username = account.as_ref().and_then(|a| a.username.clone());
        if let Some(u) = username.as_deref() {
            db.cache_wallet_username(&npub, u)?;
        }
        let account_number = account
            .and_then(|a| a.account_number)
            .or(reg.account_number);

        if json {
            println!(
                "{}",
                json!({
                    "linked": true,
                    "npub": npub,
                    "username": username,
                    "account_number": account_number,
                })
            );
        } else {
            println!("Registered {npub} with Overpay");
            if let Some(u) = username.as_deref() {
                println!("  username:       {u}");
            }
            if let Some(n) = account_number.as_deref() {
                println!("  account number: {n}");
                println!("  (your Overpay login — keep it somewhere safe)");
            }
        }
        Ok(())
    })
}
