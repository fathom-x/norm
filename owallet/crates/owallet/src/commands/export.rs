//! `owallet export key` — print the private key (or mnemonic) for a wallet.

use owallet_crypto::{derive_from_mnemonic, Mnemonic, PrivateKey, EVM_HD_PATH};
use owallet_db::default_db_path;

use super::{open_unlock, CmdError, Result};
use crate::cli::{ExportFormat, ExportWhat};

pub fn run(what: ExportWhat) -> Result<()> {
    match what {
        ExportWhat::Key { format, npub } => export_key(format, npub.as_deref()),
    }
}

fn export_key(format: ExportFormat, npub_override: Option<&str>) -> Result<()> {
    let db = open_unlock(&default_db_path())?;
    let chosen_npub = match npub_override {
        Some(s) => s.to_string(),
        None => db
            .read_default_npub()?
            .ok_or_else(|| CmdError::BadInput("no default wallet — run `owallet select`".into()))?,
    };

    let seed = db
        .read_seed(&chosen_npub)?
        .ok_or_else(|| CmdError::NotFound(chosen_npub.clone()))?;

    let (mnemonic, sk) = if seed.split_whitespace().count() >= 12 {
        let m = Mnemonic::parse(&seed)?;
        let sk = derive_from_mnemonic(&m, EVM_HD_PATH)?;
        (Some(m.phrase()), sk)
    } else {
        (None, PrivateKey::from_hex(&seed)?)
    };

    let output = match format {
        ExportFormat::Hex => sk.to_hex(),
        ExportFormat::Hex0x => format!("0x{}", sk.to_hex()),
        ExportFormat::Mnemonic => mnemonic.ok_or_else(|| {
            CmdError::BadInput(
                "this wallet was imported as a hex key — no mnemonic to export".into(),
            )
        })?,
    };

    eprintln!("{chosen_npub}:");
    println!("{output}");
    Ok(())
}
