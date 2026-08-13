//! `owallet init` — create the encrypted DB and set the master password.

use owallet_db::{default_db_path, Database};

use super::Result;
use crate::password;

pub fn run() -> Result<()> {
    let path = default_db_path();
    if Database::exists(&path) {
        println!("Database already exists at {}.", path.display());
    } else {
        println!(
            "Setting a new database password for {}. This password encrypts every seed and token\n\
             at rest. Anyone with the password and the DB file can sign as your wallet.",
            path.display()
        );
        let pw = password::read_new("Database password")?;
        let _db = Database::init(&path, pw.as_str())?;
        println!("Created encrypted database at {}.", path.display());
        println!("Next: `owallet generate` to mint a fresh seed phrase, or `owallet import` to bring an existing one in.");
    }

    Ok(())
}
