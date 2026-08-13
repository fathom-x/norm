//! `owallet select` — choose the default wallet.

use std::io::{self, BufRead, Write};

use owallet_db::{default_db_path, Database};

use super::{open_unlock, CmdError, Result};

pub fn run(identifier: Option<String>) -> Result<()> {
    let db = open_unlock(&default_db_path())?;
    let wallets = db.list_wallets()?;
    if wallets.is_empty() {
        return Err(CmdError::NoWallets);
    }

    let chosen = match identifier {
        Some(id) => db
            .find_wallet_by_identifier(&id)?
            .ok_or_else(|| CmdError::NotFound(id))?,
        None => prompt_choice(&db, &wallets)?,
    };

    db.write_default_npub(&chosen)?;
    println!("Default wallet set to {chosen}");
    Ok(())
}

fn prompt_choice(db: &Database, wallets: &[owallet_db::WalletRow]) -> Result<String> {
    let default = db.read_default_npub()?;
    println!("Wallets:");
    for (idx, w) in wallets.iter().enumerate() {
        let marker = if Some(&w.npub) == default.as_ref() {
            "*"
        } else {
            " "
        };
        let address = w.address.as_deref().unwrap_or("?");
        let username = w.overpay_username.as_deref().unwrap_or("");
        println!(
            "  {marker} [{}] {npub:<63}  {address}  {username}",
            idx + 1,
            npub = w.npub
        );
    }
    print!("Select [1-{}]: ", wallets.len());
    io::stdout().flush().ok();
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let n: usize = line
        .trim()
        .parse()
        .map_err(|_| CmdError::BadInput(format!("not a number: {line:?}")))?;
    if n == 0 || n > wallets.len() {
        return Err(CmdError::BadInput(format!("choice out of range: {n}")));
    }
    Ok(wallets[n - 1].npub.clone())
}
