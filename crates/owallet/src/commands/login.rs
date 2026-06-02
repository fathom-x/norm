//! `owallet login` — fetch a one-time web session URL using the stored
//! Overpay token and open it in the user's browser.

use owallet_db::default_db_path;

use super::overpay::{block_on, client as overpay_client, host_key};
use super::{open_unlock, CmdError, Result};

pub fn run() -> Result<()> {
    let db = open_unlock(&default_db_path())?;
    let npub = db
        .read_default_npub()?
        .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?;
    let token = db
        .read_token(&npub, &host_key())?
        .ok_or(CmdError::NotAuthorized)?;
    let overpay = overpay_client()?;

    let resp = block_on(async {
        overpay
            .web_session(owallet_overpay::Auth::Bearer(&token))
            .await
    })?;

    let url = overpay.to_public_url(&resp.url);
    println!("Opening Overpay login in your browser…");
    println!("If it doesn't open automatically, visit:\n  {url}");
    // Detached so a blocking browser-opener never stalls the command.
    let _ = open::that_detached(url);
    Ok(())
}
