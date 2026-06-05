//! CLI command implementations.

mod account;
mod authorize;
mod config;
mod credits;
mod export;
mod generate;
mod import;
mod init;
mod install;
mod list;
mod login;
mod overpay;
mod select;
mod send;
mod serve;
mod sync;
mod zcash;

use thiserror::Error;

use crate::cli::{Cli, Command, CreditsWhat};

#[derive(Debug, Error)]
pub enum CmdError {
    #[error("{0}")]
    Db(#[from] owallet_db::DbError),
    #[error("{0}")]
    Mnemonic(#[from] owallet_crypto::MnemonicError),
    #[error("{0}")]
    Hd(#[from] owallet_crypto::HdError),
    #[error("{0}")]
    Nostr(#[from] owallet_crypto::NostrError),
    #[error("{0}")]
    Config(#[from] owallet_config::ConfigError),
    #[error("{0}")]
    Overpay(#[from] owallet_overpay::OverpayError),
    #[error("{0}")]
    Evm(#[from] owallet_evm::EvmError),
    #[error("{0}")]
    Zcash(#[from] owallet_zcash::ZcashError),
    #[error("{0}")]
    Password(#[from] crate::password::PasswordError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("bad input: {0}")]
    BadInput(String),
    #[error("wrong password")]
    WrongPassword,
    #[error("no wallets stored")]
    NoWallets,
    #[error("wallet not found: {0}")]
    NotFound(String),
    #[error("not authorized — run `owallet authorize` to link this wallet to Overpay")]
    NotAuthorized,
    #[error("oauth callback: {0}")]
    OauthCallback(String),
}

pub type Result<T> = std::result::Result<T, CmdError>;

/// Open the DB at `path` and unlock it with the wallet password (env or TTY
/// prompt). Returns [`CmdError::WrongPassword`] if the prompt+verify fails.
pub(crate) fn open_unlock(path: &std::path::Path) -> Result<owallet_db::Database> {
    let mut db = owallet_db::Database::open(path)?;
    let pw = crate::password::read("Database password")?;
    if !db.unlock(pw.as_str())? {
        return Err(CmdError::WrongPassword);
    }
    Ok(db)
}

pub fn dispatch(args: Cli) -> Result<()> {
    // Two commands consume the global --prod/--dev/--staging flags
    // directly (multi-config support); dispatch them before consuming
    // `args.command`.
    if let Command::Serve { ref port, ref host } = args.command {
        return serve::run_with_cli(&args, port.clone(), host.clone());
    }
    if let Command::Install {
        claude_local,
        claude_global,
        opencode_local,
        opencode_global,
        codex_local,
        codex_global,
        port,
    } = &args.command
    {
        return install::run(install::InstallArgs {
            claude_local: *claude_local,
            claude_global: *claude_global,
            opencode_local: *opencode_local,
            opencode_global: *opencode_global,
            codex_local: *codex_local,
            codex_global: *codex_global,
            port: *port,
            cli: &args,
        });
    }
    match args.command {
        Command::Init => init::run(),
        Command::Account { all } => account::run(all),
        Command::Serve { .. } | Command::Install { .. } => unreachable!(),
        Command::Generate { words } => generate::run(words),
        Command::Import {
            mnemonic,
            private_key,
            zec_birthday,
        } => import::run(mnemonic, private_key, zec_birthday),
        Command::Select { identifier } => select::run(identifier),
        Command::Export { what } => export::run(what),
        Command::Authorize => authorize::run(),
        Command::Login => login::run(),
        Command::List { what } => list::run(what),
        Command::Send { to, amount, asset } => send::run(&to, amount, asset),
        Command::Sync => sync::run(),
        Command::Config { mcp } => config::run(mcp),
        Command::Credits { what } => match what {
            CreditsWhat::Load { amount_cents, wait } => credits::run(amount_cents, wait),
        },
    }
}
