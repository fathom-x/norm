//! Password prompting / sourcing.
//!
//! Mirrors `_prompt_password` in `wallet_mcp/cli.py:37-42`: prefers the
//! `OWALLET_PASSWORD` environment variable, otherwise prompts on the TTY
//! with no echo.

use thiserror::Error;
use zeroize::Zeroize;

#[derive(Debug, Error)]
pub enum PasswordError {
    #[error("password prompt failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("password confirmation did not match")]
    Mismatch,
    #[error("wallet password cannot be empty")]
    Empty,
}

/// A password held as a `String` that zeroizes its contents on drop.
pub struct Password(String);

impl Password {
    pub fn from_string(s: String) -> Self {
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Drop for Password {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Read the wallet password: env var first, otherwise TTY prompt.
pub fn read(prompt: &str) -> Result<Password, PasswordError> {
    if let Ok(p) = std::env::var("OWALLET_PASSWORD") {
        return Ok(Password::from_string(p));
    }
    let entered = rpassword::prompt_password(format!("{prompt}: "))?;
    Ok(Password::from_string(entered))
}

/// Prompt for a brand-new per-wallet password (used to log into the web
/// admin) and confirm it. Matches the wallet-password prompt in
/// `wallet_mcp/cli.py` (generate / import): non-empty required, must match.
///
/// `OWALLET_WALLET_PASSWORD` short-circuits the prompt for non-interactive
/// use (scripts, CI). This is the *wallet* password — distinct from the
/// *database* password `OWALLET_PASSWORD`. The env fallback is a Rust-only
/// convenience (Python always prompts); interactive behaviour is identical.
/// It also dodges `rpassword`'s `/dev/tty`-only input, which can't be driven
/// from a piped stdin in tests.
pub fn read_new_wallet_password() -> Result<Password, PasswordError> {
    if let Ok(p) = std::env::var("OWALLET_WALLET_PASSWORD") {
        if p.is_empty() {
            return Err(PasswordError::Empty);
        }
        return Ok(Password::from_string(p));
    }
    let first =
        rpassword::prompt_password("Choose a wallet password (used to log into the web admin): ")?;
    if first.is_empty() {
        return Err(PasswordError::Empty);
    }
    let confirm = rpassword::prompt_password("Confirm wallet password: ")?;
    if first != confirm {
        let mut a = first;
        let mut b = confirm;
        a.zeroize();
        b.zeroize();
        return Err(PasswordError::Mismatch);
    }
    Ok(Password::from_string(first))
}

/// Read a new password twice and require the two entries to match.
/// `OWALLET_PASSWORD` short-circuits the second prompt for non-interactive use.
pub fn read_new(prompt: &str) -> Result<Password, PasswordError> {
    if let Ok(p) = std::env::var("OWALLET_PASSWORD") {
        return Ok(Password::from_string(p));
    }
    let first = rpassword::prompt_password(format!("{prompt}: "))?;
    let confirm = rpassword::prompt_password("Confirm: ")?;
    if first != confirm {
        // Wipe both copies before returning.
        let mut a = first;
        let mut b = confirm;
        a.zeroize();
        b.zeroize();
        return Err(PasswordError::Mismatch);
    }
    Ok(Password::from_string(first))
}
