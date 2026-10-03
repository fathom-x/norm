//! Wallet creation shared by every front end: `owallet generate` /
//! `owallet import` and the browser build's `/_mgmt/generate` / `import`.
//!
//! Two steps on purpose. [`PreparedWallet`] derives everything in memory
//! only; [`crate::Database::store_wallet`] persists it. A front end that
//! still needs input (the CLI's per-wallet password prompt) asks between
//! the two, so an abandoned prompt leaves the database untouched.

use owallet_crypto::{
    derive_from_mnemonic, npub_from_private_key, Address, HdError, Mnemonic, MnemonicError,
    NostrError, PrivateKey, WordCount, EVM_HD_PATH,
};

/// Why a wallet secret could not be turned into a wallet.
#[derive(Debug, thiserror::Error)]
pub enum NewWalletError {
    #[error(transparent)]
    Mnemonic(#[from] MnemonicError),
    #[error(transparent)]
    Hd(#[from] HdError),
    #[error(transparent)]
    Nostr(#[from] NostrError),
}

/// A wallet derived in memory, not yet stored.
pub struct PreparedWallet {
    /// What goes into the encrypted `wallets` row: the BIP-39 phrase, or
    /// `0x<hex>` for a raw private key.
    pub stored_seed: String,
    pub sk: PrivateKey,
    pub address: Address,
    pub npub: String,
}

impl PreparedWallet {
    /// A fresh BIP-39 wallet.
    pub fn generate(words: WordCount) -> Result<Self, NewWalletError> {
        Self::from_mnemonic(&Mnemonic::generate(words))
    }

    /// An existing BIP-39 phrase.
    pub fn from_phrase(phrase: &str) -> Result<Self, NewWalletError> {
        Self::from_mnemonic(&Mnemonic::parse(phrase)?)
    }

    /// An existing hex private key (`0x` optional).
    pub fn from_private_key_hex(hex: &str) -> Result<Self, NewWalletError> {
        let sk = PrivateKey::from_hex(hex)?;
        Self::from_key(format!("0x{}", sk.to_hex()), sk)
    }

    /// A phrase (12+ words) or a hex private key — whichever `secret` looks
    /// like, as the CLI's hidden prompt accepts.
    pub fn from_secret(secret: &str) -> Result<Self, NewWalletError> {
        let trimmed = secret.trim();
        if trimmed.split_whitespace().count() >= 12 {
            Self::from_phrase(trimmed)
        } else {
            Self::from_private_key_hex(trimmed)
        }
    }

    fn from_mnemonic(m: &Mnemonic) -> Result<Self, NewWalletError> {
        let sk = derive_from_mnemonic(m, EVM_HD_PATH)?;
        Self::from_key(m.phrase(), sk)
    }

    fn from_key(stored_seed: String, sk: PrivateKey) -> Result<Self, NewWalletError> {
        let address = Address::from_private_key(&sk);
        let npub = npub_from_private_key(&sk)?;
        Ok(Self {
            stored_seed,
            sk,
            address,
            npub,
        })
    }
}
