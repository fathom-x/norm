//! BIP-39 mnemonic generation and parsing.
//!
//! Mirrors `eth_account.Account.create_with_mnemonic(num_words)` and
//! `Account.from_mnemonic` from the Python implementation.

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordCount {
    Twelve,
    TwentyFour,
}

impl WordCount {
    fn entropy_bytes(self) -> usize {
        match self {
            // 128 bits of entropy → 12 words; 256 bits → 24 words.
            Self::Twelve => 16,
            Self::TwentyFour => 32,
        }
    }
}

#[derive(Debug, Error)]
pub enum MnemonicError {
    #[error("invalid mnemonic: {0}")]
    Invalid(String),
}

/// A BIP-39 mnemonic. The phrase is held as a `String` for ergonomics; the
/// underlying `bip39::Mnemonic` validates the wordlist + checksum on parse.
pub struct Mnemonic(bip39::Mnemonic);

impl Mnemonic {
    /// Generate a fresh mnemonic from cryptographic randomness.
    pub fn generate(words: WordCount) -> Self {
        let mut entropy = vec![0u8; words.entropy_bytes()];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut entropy);
        let m = bip39::Mnemonic::from_entropy(&entropy).expect("entropy length is always 16 or 32");
        Self(m)
    }

    /// Parse a space-separated BIP-39 phrase. Validates wordlist + checksum.
    pub fn parse(phrase: &str) -> Result<Self, MnemonicError> {
        let m = bip39::Mnemonic::parse_normalized(phrase.trim())
            .map_err(|e| MnemonicError::Invalid(e.to_string()))?;
        Ok(Self(m))
    }

    /// Space-separated phrase form.
    pub fn phrase(&self) -> String {
        self.0.to_string()
    }

    /// BIP-39 64-byte seed. The passphrase parameter is the BIP-39 "salt"
    /// (appended to "mnemonic" before PBKDF2-SHA512). Empty by default,
    /// matching `eth_account`'s default behaviour.
    pub fn to_seed(&self, passphrase: &str) -> [u8; 64] {
        self.0.to_seed_normalized(passphrase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical "abandon abandon ..." 12-word test vector, used
    /// throughout the BIP-39 spec and reproduced in test_db.py.
    const ABANDON_12: &str = "abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon about";

    #[test]
    fn generate_12_words() {
        let m = Mnemonic::generate(WordCount::Twelve);
        assert_eq!(m.phrase().split_whitespace().count(), 12);
    }

    #[test]
    fn generate_24_words() {
        let m = Mnemonic::generate(WordCount::TwentyFour);
        assert_eq!(m.phrase().split_whitespace().count(), 24);
    }

    #[test]
    fn parse_roundtrip() {
        let m = Mnemonic::parse(ABANDON_12).unwrap();
        assert_eq!(m.phrase(), ABANDON_12);
    }

    #[test]
    fn parse_rejects_bad_checksum() {
        // Swap last word for one that doesn't match the checksum bits.
        let bad = ABANDON_12.replace("about", "abandon");
        assert!(Mnemonic::parse(&bad).is_err());
    }

    #[test]
    fn parse_rejects_unknown_word() {
        let bad = ABANDON_12.replace("about", "notaword");
        assert!(Mnemonic::parse(&bad).is_err());
    }

    // Reference seed from the BIP-39 specification test vectors:
    // https://github.com/trezor/python-mnemonic/blob/master/vectors.json
    #[test]
    fn seed_matches_bip39_test_vector() {
        let m = Mnemonic::parse(ABANDON_12).unwrap();
        let seed = m.to_seed("TREZOR");
        let expected = "c55257c360c07c72029aebc1b53c05ed0362ada38ead3e3e9efa3708e53495531f09a6987599d18264c1e1c92f2cf141630c7a3c4ab7c81b2f001698e7463b04";
        assert_eq!(hex::encode(seed), expected);
    }
}
