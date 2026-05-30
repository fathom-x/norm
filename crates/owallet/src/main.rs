//! owallet — the Overpay wallet CLI (Rust port of `wallet_mcp/cli.py`).
//!
//! Offline subcommands implemented in this binary: `init`, `generate`,
//! `import`, `select`, `export`, `account` (offline view), `config`.
//! HTTP-dependent subcommands (`serve`, `authorize`, `login`, `migrate`,
//! `install`, `buy`, `list marketplace`) land in later phases.

mod cli;
mod commands;
mod password;

use std::process::ExitCode;

use clap::Parser;

fn main() -> ExitCode {
    let args = cli::Cli::parse();

    // Resolve `.owallet` configs and load them into the process environment
    // before any subcommand reads env vars.
    if let Err(e) = cli::load_env_from_flags(&args) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }

    match commands::dispatch(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
