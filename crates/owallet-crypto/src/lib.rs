//! Cryptographic primitives for owallet.
//!
//! Byte-compatible with the Python `wallet_mcp` implementation:
//! - PBKDF2-HMAC-SHA256 with 600,000 iterations for the AES key (and count=1 for the verify hash)
//! - AES-256-GCM with 16-byte nonces and 16-byte authentication tags appended to ciphertext
//! - BIP-39 mnemonics, BIP-44 derivation at `m/44'/60'/0'/0/0`
//! - Nostr `npub` (bech32 of the x-only secp256k1 public key)
//! - NIP-98 HTTP authentication via signed kind-27235 Nostr events

pub mod aesgcm;
pub mod bip39;
pub mod evm;
pub mod hd;
pub mod kdf;
pub mod nip98;
pub mod nostr;
pub mod statekey;

pub use aesgcm::{decrypt, encrypt, AesKey, DecryptError};
pub use bip39::{Mnemonic, MnemonicError, WordCount};
pub use evm::Address;
pub use hd::{
    bip39_seed_from_stored, derive_from_mnemonic, derive_from_seed, derive_from_stored_seed,
    HdError, PrivateKey, EVM_HD_PATH,
};
pub use kdf::{derive_key, hashes_equal, verify_hash, KDF_ITERATIONS, KEY_LEN, SALT_LEN};
pub use nostr::{npub_decode, npub_encode, npub_from_private_key, xonly_pubkey, NostrError};
pub use statekey::derive_state_key;
