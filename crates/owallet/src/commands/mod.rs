//! CLI command implementations.

mod account;
mod authorize;
mod balance;
mod config;
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

use std::path::{Path, PathBuf};

use owallet_config::search_dirs;
use thiserror::Error;

use crate::cli::{Cli, Command};

const SCAFFOLD_ENVS: [(&str, &str, &str); 3] = [
    (
        "prod",
        "prod.owallet",
        "OVERPAY_RAILS_URL=https://overpay.com\nOWALLET_PORT=8765\n",
    ),
    (
        "dev",
        "dev.owallet",
        "OVERPAY_RAILS_URL=http://localhost:3001\nOWALLET_PORT=8766\n",
    ),
    (
        "staging",
        "staging.owallet",
        "# Set OVERPAY_RAILS_URL to your staging instance\nOWALLET_PORT=8767\n",
    ),
];

/// Scaffold missing `.owallet` config files next to the binary (or in
/// `$OWALLET_CONFIG_DIR`). `active` = [prod, dev, staging]; pass all `false`
/// (e.g. from `init`) to scaffold every env.
pub(crate) fn scaffold_owallet_configs(active: [bool; 3]) -> Result<()> {
    let dir = {
        let from_env = std::env::var_os("OWALLET_CONFIG_DIR")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from);
        let from_exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        match from_env.or(from_exe) {
            Some(d) => d,
            None => return Ok(()),
        }
    };

    let any_explicit = active.iter().any(|b| *b);
    let search = search_dirs();

    for (i, (_env, filename, content)) in SCAFFOLD_ENVS.iter().enumerate() {
        if any_explicit && !active[i] {
            continue;
        }
        if search.iter().any(|d| d.join(filename).exists()) {
            continue;
        }
        let dest = dir.join(filename);
        std::fs::write(&dest, content)?;
        println!("Created {}", dest.display());
    }

    Ok(())
}

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
        Command::Balance => balance::run(),
        Command::Account { all } => account::run(all),
        Command::Serve { .. } | Command::Install { .. } => unreachable!(),
        Command::Generate { words } => generate::run(words),
        Command::Import {
            mnemonic,
            private_key,
        } => import::run(mnemonic, private_key),
        Command::Select { identifier } => select::run(identifier),
        Command::Export { what } => export::run(what),
        Command::Authorize => authorize::run(),
        Command::Login => login::run(),
        Command::List { what } => list::run(what),
        Command::Send { to, amount } => send::run(&to, amount),
        Command::Config { mcp } => config::run(mcp),
    }
}
