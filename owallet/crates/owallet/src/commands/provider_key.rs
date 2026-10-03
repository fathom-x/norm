//! `owallet provider-key` — mint and list the wallet-scoped keys that
//! authenticate the `/v1` OpenAI-compatible endpoint, without going
//! through the dashboard or the browser OAuth flow. norm's bootstrap
//! consumes `create --json` to provision OpenCode's auth store
//! non-interactively.

use owallet_db::{default_db_path, parse_budget_usd, MintProviderKeyError};

use super::{open_unlock, CmdError, Result};
use crate::cli::ProviderKeyWhat;

pub fn run(what: ProviderKeyWhat) -> Result<()> {
    match what {
        ProviderKeyWhat::Create {
            label,
            spend,
            budget_usd,
            npub,
            json,
        } => create(&label, spend, budget_usd.as_deref(), npub.as_deref(), json),
        ProviderKeyWhat::List { npub } => list(npub.as_deref()),
    }
}

fn resolve_npub(db: &owallet_db::Database, npub_override: Option<&str>) -> Result<String> {
    match npub_override {
        Some(s) => Ok(s.to_string()),
        None => db
            .read_default_npub()?
            .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into())),
    }
}

fn create(
    label: &str,
    spend: bool,
    budget_usd: Option<&str>,
    npub_override: Option<&str>,
    json: bool,
) -> Result<()> {
    // Same scope semantics as the dashboard create form: `spend` is only
    // ever granted by an explicit user choice, here the --spend flag.
    let budget_usd_cents =
        parse_budget_usd(budget_usd).map_err(|e| CmdError::BadInput(e.into()))?;

    let db = open_unlock(&default_db_path())?;
    let minted = db
        .mint_provider_key(npub_override, label, spend, budget_usd_cents)
        .map_err(|e| match e {
            MintProviderKeyError::NoDefaultWallet => {
                CmdError::BadInput("no default wallet — run `owallet select`".into())
            }
            MintProviderKeyError::UnknownWallet(npub) => CmdError::NotFound(npub),
            MintProviderKeyError::Db(e) => e.into(),
        })?;

    if json {
        println!("{}", minted.to_json());
    } else {
        let (npub, scopes, key) = (&minted.npub, &minted.scopes, &minted.key);
        eprintln!("Provider key for {npub} (scopes: {scopes}):");
        println!("{key}");
        eprintln!("This key is shown once only — owallet stores just a verifier.");
    }
    Ok(())
}

fn list(npub_override: Option<&str>) -> Result<()> {
    let db = open_unlock(&default_db_path())?;
    let npub = resolve_npub(&db, npub_override)?;
    let rows = db.list_provider_keys(&npub)?;
    if rows.is_empty() {
        println!("No provider keys for {npub}.");
        return Ok(());
    }
    println!("Provider keys for {npub}:");
    for row in rows {
        let prefix = row.token_prefix.as_deref().unwrap_or("owk_????????");
        let label = row.label.as_deref().unwrap_or("-");
        let scopes = row.scopes.as_deref().unwrap_or("chat");
        let budget = match row.daily_budget_usd_cents {
            Some(cents) => format!(
                "${:.2}/day (${:.2} spent today)",
                cents as f64 / 100.0,
                row.spent_today_usd_cents() as f64 / 100.0
            ),
            None => "no limit".to_string(),
        };
        println!("  {prefix}…  {label}  [{scopes}]  {budget}");
    }
    Ok(())
}
