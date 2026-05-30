//! BIP-32 hierarchical deterministic derivation.
//!
//! Derives the leaf EVM key at the BIP-44 path used throughout the Python
//! implementation: `m/44'/60'/0'/0/0` (see `EVM_HD_PATH` in
//! `wallet_mcp/server.py:55`).

use thiserror::Error;
use zeroize::Zeroize;

use crate::bip39::Mnemonic;

/// Default BIP-44 derivation path for EVM accounts.
pub const EVM_HD_PATH: &str = "m/44'/60'/0'/0/0";

#[derive(Debug, Error)]
pub enum HdError {
    #[error("invalid derivation path '{path}': {source}")]
    BadPath {
        path: String,
        #[source]
        source: bip32::Error,
    },
    #[error("HD derivation failed: {0}")]
    Derivation(#[from] bip32::Error),
    #[error("stored seed is not a valid BIP-39 mnemonic or hex key: {0}")]
    BadSeed(String),
}

/// 32-byte secp256k1 private key, zeroed on drop.
#[derive(Clone, Zeroize)]
#[zeroize(drop)]
pub struct PrivateKey(pub [u8; 32]);

impl PrivateKey {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a hex-encoded private key with optional `0x` prefix.
    pub fn from_hex(s: &str) -> Result<Self, HdError> {
        let s = s.trim();
        let stripped = s
            .strip_prefix("0x")
            .or_else(|| s.strip_prefix("0X"))
            .unwrap_or(s);
        let bytes = hex::decode(stripped).map_err(|_| HdError::Derivation(bip32::Error::Decode))?;
        let arr: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| HdError::Derivation(bip32::Error::Decode))?;
        Ok(Self(arr))
    }
}

/// Derive the EVM leaf private key from a mnemonic at `path`.
///
/// Equivalent to `Account.from_mnemonic(mnemonic, account_path=path)` in
/// eth_account.
pub fn derive_from_mnemonic(mnemonic: &Mnemonic, path: &str) -> Result<PrivateKey, HdError> {
    let seed = mnemonic.to_seed("");
    derive_from_seed(&seed, path)
}

pub fn derive_from_seed(seed: &[u8], path: &str) -> Result<PrivateKey, HdError> {
    let parsed: bip32::DerivationPath = path.parse().map_err(|e| HdError::BadPath {
        path: path.to_string(),
        source: e,
    })?;
    let xprv = bip32::XPrv::derive_from_path(seed, &parsed)?;
    let mut key_bytes = xprv.private_key().to_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&key_bytes);
    // Zeroize the intermediate buffer.
    key_bytes.zeroize();
    Ok(PrivateKey(out))
}

/// Recover the EVM private key from a seed previously stored by
/// `owallet-db`. The DB stores either a BIP-39 mnemonic phrase (12+ words)
/// or a hex-encoded private key (with optional `0x` prefix). This helper
/// picks the right path based on a whitespace heuristic.
pub fn derive_from_stored_seed(seed: &str) -> Result<PrivateKey, HdError> {
    if seed.split_whitespace().count() >= 12 {
        let m = crate::bip39::Mnemonic::parse(seed).map_err(|e| HdError::BadSeed(e.to_string()))?;
        derive_from_mnemonic(&m, EVM_HD_PATH)
    } else {
        PrivateKey::from_hex(seed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABANDON_12: &str = "abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon about";

    /// The standard "abandon ... about" mnemonic derives a well-known EVM
    /// private key at `m/44'/60'/0'/0/0`. This value is documented across
    /// many Ethereum HD wallet implementations (eth_account, ethers-rs, etc.)
    /// and produces address `0x9858EfFD232B4033E47d90003D41EC34EcaEda94`.
    #[test]
    fn abandon_evm_path_matches_known_key() {
        let m = Mnemonic::parse(ABANDON_12).unwrap();
        let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
        assert_eq!(
            sk.to_hex(),
            "1ab42cc412b618bdea3a599e3c9bae199ebf030895b039e9db1e30dafb12b727"
        );
    }

    #[test]
    fn bad_path_returns_error() {
        let m = Mnemonic::parse(ABANDON_12).unwrap();
        assert!(derive_from_mnemonic(&m, "not/a/valid/path").is_err());
    }

    #[test]
    fn deterministic_across_calls() {
        let m = Mnemonic::parse(ABANDON_12).unwrap();
        let a = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
        let b = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
        assert_eq!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn different_paths_yield_different_keys() {
        let m = Mnemonic::parse(ABANDON_12).unwrap();
        let a = derive_from_mnemonic(&m, "m/44'/60'/0'/0/0").unwrap();
        let b = derive_from_mnemonic(&m, "m/44'/60'/0'/0/1").unwrap();
        assert_ne!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn from_hex_strips_0x_prefix() {
        let without_str = "ab".repeat(32);
        let with_str = format!("0x{without_str}");
        let with = PrivateKey::from_hex(&with_str).unwrap();
        let without = PrivateKey::from_hex(&without_str).unwrap();
        assert_eq!(with.to_hex(), without.to_hex());
    }

    #[test]
    fn from_hex_rejects_wrong_length() {
        assert!(PrivateKey::from_hex("ab".repeat(31).as_str()).is_err());
    }
}
