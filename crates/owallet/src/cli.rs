//! Clap-derived CLI definition and environment-flag plumbing.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use owallet_config::{resolve, ConfigSelector};

#[derive(Debug, Parser)]
#[command(
    name = "owallet",
    version,
    about = "Overpay wallet CLI",
    propagate_version = true
)]
pub struct Cli {
    /// Path to an explicit `.owallet` config file (required if missing).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Load `prod.owallet` (searched in $OWALLET_CONFIG_DIR, the current
    /// directory, then the executable's directory).
    #[arg(long, global = true)]
    pub prod: bool,

    /// Load `dev.owallet` (searched in $OWALLET_CONFIG_DIR, the current
    /// directory, then the executable's directory).
    #[arg(long, global = true)]
    pub dev: bool,

    /// Load `staging.owallet` (searched in $OWALLET_CONFIG_DIR, the current
    /// directory, then the executable's directory).
    #[arg(long, global = true)]
    pub staging: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize an encrypted wallet database at $OWALLET_DB_PATH or ~/.owallet.db.
    Init,

    /// Run one or more HTTP servers (dashboard + OAuth AS + /mcp).
    ///
    /// With no `--prod/--dev/--staging` flags, a single server runs using
    /// the process env (or built-in defaults). With multiple flags, one
    /// server is spawned per `.owallet` config — each one a separate
    /// tokio task on its own bind port.
    Serve {
        /// Override bind ports. Single value applies to the lone server;
        /// `9001,9002` maps positionally to the active configs.
        #[arg(long)]
        port: Option<String>,
        /// Bind host (overrides OWALLET_HOST). Applied to every server.
        #[arg(long)]
        host: Option<String>,
    },

    /// Generate a fresh BIP-39 seed phrase and store it.
    Generate {
        /// Mnemonic length: 12 or 24 words.
        #[arg(long, default_value_t = 24)]
        words: u8,
    },

    /// Import an existing BIP-39 mnemonic or hex private key.
    Import {
        /// Optional inline phrase or hex key. If omitted, the value is read
        /// from stdin (with prompt).
        #[arg(long)]
        mnemonic: Option<String>,

        /// Optional inline hex private key (with or without 0x prefix).
        #[arg(long, conflicts_with = "mnemonic")]
        private_key: Option<String>,
    },

    /// Pick which stored wallet is the default. With no argument, lists
    /// wallets interactively.
    Select {
        /// Wallet identifier (npub, address, or cached Overpay username).
        #[arg(value_name = "WALLET")]
        identifier: Option<String>,
    },

    /// Export key material for the default wallet (or `--npub <npub>`).
    Export {
        #[command(subcommand)]
        what: ExportWhat,
    },

    /// Show wallet metadata + (when a token is stored) the linked Overpay
    /// account info fetched live from the Rails API.
    Account {
        /// Show info for every stored wallet, not just the default.
        #[arg(long)]
        all: bool,
    },

    /// Link this wallet to an Overpay account via the OAuth PKCE flow.
    /// Opens the browser; spins up a local callback server on a free port.
    Authorize,

    /// Open the Overpay web UI using the stored OAuth token (one-time
    /// session URL).
    Login,

    /// Browse the Overpay marketplace.
    List {
        #[command(subcommand)]
        what: ListWhat,
    },

    /// Sign and broadcast an ERC-20 USDC transfer on the configured EVM
    /// chain (default Base mainnet). EVM_RPC_URL + EVM_NETWORK override
    /// the defaults.
    Send {
        /// Recipient EVM address (0x…).
        #[arg(long)]
        to: String,
        /// Amount of USDC to send (e.g. 1.25).
        #[arg(long)]
        amount: f64,
    },

    /// Show resolved URL/port configuration (or, with `--mcp`, the MCP
    /// install JSON blob).
    Config {
        /// Print the `.mcp.json` blob for installing into an MCP client.
        #[arg(long)]
        mcp: bool,
    },

    /// Register owallet (+ the hosted Overpay MCP) with an MCP client
    /// config file. Targets one or more of Claude Code, OpenCode, Codex,
    /// at either local-project or user-global scope.
    Install {
        #[arg(long)]
        claude_local: bool,
        #[arg(long)]
        claude_global: bool,
        #[arg(long)]
        opencode_local: bool,
        #[arg(long)]
        opencode_global: bool,
        #[arg(long)]
        codex_local: bool,
        #[arg(long)]
        codex_global: bool,

        /// Override the owallet MCP port (default: OWALLET_PORT or 8765).
        #[arg(long)]
        port: Option<u16>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ListWhat {
    /// Show marketplace listings.
    Marketplace {
        /// Filter by category.
        #[arg(long)]
        category: Option<String>,
        /// Filter by seller slug.
        #[arg(long)]
        seller: Option<String>,
        /// Continuation cursor returned from a previous page.
        #[arg(long)]
        cursor: Option<String>,
        /// Max results per page.
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
}

#[derive(Debug, Subcommand)]
pub enum ExportWhat {
    /// Print the private key (default format: 0x-prefixed hex).
    Key {
        /// Output format.
        #[arg(long, value_enum, default_value_t = ExportFormat::Hex0x)]
        format: ExportFormat,

        /// Choose a non-default wallet by npub.
        #[arg(long)]
        npub: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum ExportFormat {
    /// Lowercase hex (no prefix).
    Hex,
    /// Lowercase hex with `0x` prefix (default).
    Hex0x,
    /// The original BIP-39 mnemonic if the wallet was stored as one.
    Mnemonic,
}

/// Resolve any `--config`/`--prod`/`--dev`/`--staging` flags and load the
/// resulting `.owallet` files into the process environment.
pub fn load_env_from_flags(args: &Cli) -> Result<(), owallet_config::ConfigError> {
    let selector = config_selector(args);
    // When `serve` is going to run with multiple configs, we deliberately
    // don't pollute the process env: each server reads its own `.owallet`
    // in isolation. For every other command (and single-config `serve`)
    // the standard env-population behaviour is correct.
    if let Command::Serve { .. } = args.command {
        if multi_config(args) {
            return Ok(());
        }
    }
    let paths = resolve(&selector)?;
    // Single active config: resolve the per-environment `OVERPAY_*_<POSTFIX>`
    // env vars into their unsuffixed forms (and clear any stale unsuffixed
    // ones) before loading the file — matches `_apply_env_overrides` in
    // `wallet_mcp/cli.py`, which runs only when exactly one config is active.
    // With zero configs the generic env var is left untouched.
    if paths.len() == 1 {
        owallet_config::apply_env_overrides(&owallet_config::env_postfix(&paths[0]));
    }
    owallet_config::load_into_env(&paths)
}

pub fn config_selector(args: &Cli) -> ConfigSelector {
    ConfigSelector {
        explicit: args.config.clone(),
        prod: args.prod,
        dev: args.dev,
        staging: args.staging,
        repo_root: None,
    }
}

/// True when the user asked for more than one `.owallet` config.
pub fn multi_config(args: &Cli) -> bool {
    [args.prod, args.dev, args.staging]
        .iter()
        .filter(|b| **b)
        .count()
        > 1
}
