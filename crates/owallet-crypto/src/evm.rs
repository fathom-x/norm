//! EVM address derivation from a secp256k1 private key.
//!
//! Mirrors `eth_account.Account.from_key(sk).address`: the address is the
//! last 20 bytes of `keccak256(uncompressed_pubkey_without_prefix)`,
//! formatted as a `0x`-prefixed lowercase hex string with an EIP-55
//! checksum (mixed case).

use secp256k1::{PublicKey, Secp256k1, SecretKey};
use tiny_keccak::{Hasher, Keccak};

use crate::hd::PrivateKey;

/// A 20-byte EVM address. Display formats it as EIP-55 mixed-case hex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address(pub [u8; 20]);

impl Address {
    /// Convert a private key into the corresponding EVM address.
    pub fn from_private_key(sk: &PrivateKey) -> Self {
        let secp = Secp256k1::new();
        let secret =
            SecretKey::from_slice(sk.as_bytes()).expect("PrivateKey enforces 32-byte input");
        let pk = PublicKey::from_secret_key(&secp, &secret);
        // The 65-byte SEC1 uncompressed form is `04 || X || Y`; address
        // input is just `X || Y` (the trailing 64 bytes).
        let uncompressed = pk.serialize_uncompressed();
        debug_assert_eq!(uncompressed[0], 0x04);
        Self::from_pubkey_xy(&uncompressed[1..])
    }

    fn from_pubkey_xy(xy: &[u8]) -> Self {
        debug_assert_eq!(xy.len(), 64);
        let mut hasher = Keccak::v256();
        hasher.update(xy);
        let mut hash = [0u8; 32];
        hasher.finalize(&mut hash);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&hash[12..]);
        Self(addr)
    }

    /// Lowercase `0x`-prefixed hex form.
    #[must_use]
    pub fn to_hex_lower(&self) -> String {
        format!("0x{}", hex::encode(self.0))
    }

    /// EIP-55 checksum-encoded mixed-case hex form.
    #[must_use]
    pub fn to_checksum(&self) -> String {
        let lower = hex::encode(self.0);
        let mut hasher = Keccak::v256();
        hasher.update(lower.as_bytes());
        let mut hash = [0u8; 32];
        hasher.finalize(&mut hash);

        let mut out = String::with_capacity(42);
        out.push_str("0x");
        for (i, ch) in lower.chars().enumerate() {
            // The checksum bit lives in the high nibble of byte `i/2`
            // when `i` is even, and the low nibble when odd.
            let nibble = (hash[i / 2] >> (4 * (1 - (i % 2)) as u32)) & 0x0f;
            if ch.is_ascii_alphabetic() && nibble >= 8 {
                out.push(ch.to_ascii_uppercase());
            } else {
                out.push(ch);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bip39::Mnemonic;
    use crate::hd::{derive_from_mnemonic, EVM_HD_PATH};

    const ABANDON_12: &str = "abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon about";

    /// Well-known address for the "abandon … about" mnemonic at the standard
    /// Ethereum BIP-44 path. Cross-checked against the Python
    /// `eth_account.Account.from_mnemonic(...).address` output.
    #[test]
    fn abandon_path_zero_address() {
        let m = Mnemonic::parse(ABANDON_12).unwrap();
        let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
        let addr = Address::from_private_key(&sk);
        assert_eq!(
            addr.to_hex_lower(),
            "0x9858effd232b4033e47d90003d41ec34ecaeda94"
        );
    }

    /// EIP-55 checksum vector from the spec.
    /// https://eips.ethereum.org/EIPS/eip-55
    #[test]
    fn eip55_checksum_vector_fb6c() {
        let lower = "0xfb6916095ca1df60bb79ce92ce3ea74c37c5d359";
        let bytes = hex::decode(&lower[2..]).unwrap();
        let mut arr = [0u8; 20];
        arr.copy_from_slice(&bytes);
        let addr = Address(arr);
        assert_eq!(
            addr.to_checksum(),
            "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359"
        );
    }
}
