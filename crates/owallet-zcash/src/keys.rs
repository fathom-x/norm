//! Orchard key derivation. Bridges the owallet BIP-39 seed to a librustzcash
//! Unified Spending Key and derives the Orchard-only Unified Address used for
//! receiving.

use std::str::FromStr;

use zcash_address::ZcashAddress;
use zcash_keys::keys::{UnifiedAddressRequest, UnifiedSpendingKey};
use zcash_protocol::consensus;
use zip32::AccountId;

use crate::{error::ZcashError, network::Network};

/// Single-account-per-wallet: account index 0 (matches zkv).
pub(crate) const ACCOUNT: AccountId = AccountId::ZERO;

/// Derive the Unified Spending Key for this wallet's single account from the
/// raw 64-byte BIP-39 seed. Regenerated on the fly whenever signing is needed;
/// never persisted.
pub(crate) fn spending_key(
    network: Network,
    seed: &[u8; 64],
) -> Result<UnifiedSpendingKey, ZcashError> {
    let net: consensus::Network = network.into();
    UnifiedSpendingKey::from_seed(&net, seed, ACCOUNT)
        .map_err(|e| ZcashError::Backend(format!("derive spending key: {e:?}")))
}

/// Derive the Orchard-only Unified Address (the receive address) from the seed.
/// Offline — no network access.
pub fn orchard_ua_from_seed(network: Network, seed: &[u8; 64]) -> Result<String, ZcashError> {
    let net: consensus::Network = network.into();
    let usk = spending_key(network, seed)?;
    let ufvk = usk.to_unified_full_viewing_key();
    let (ua, _) = ufvk
        .default_address(UnifiedAddressRequest::ORCHARD)
        .map_err(|e| ZcashError::Backend(format!("derive Orchard UA: {e:?}")))?;
    Ok(ua.encode(&net))
}

/// True if `s` parses as any Zcash address (Unified, Sapling, or transparent).
/// Used by the buy/pay router to distinguish a Zcash recipient from an EVM
/// `0x…` address.
#[must_use]
pub fn is_zcash_address(s: &str) -> bool {
    ZcashAddress::from_str(s).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use owallet_crypto::bip39_seed_from_stored;

    const ABANDON_12: &str = "abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon about";

    #[test]
    fn orchard_ua_is_deterministic_and_well_formed() {
        let seed = bip39_seed_from_stored(ABANDON_12).unwrap();
        let ua_main = orchard_ua_from_seed(Network::Main, &seed).unwrap();
        // Mainnet Orchard UAs are bech32m with the `u1` HRP.
        assert!(ua_main.starts_with("u1"), "got {ua_main}");
        // Deterministic.
        assert_eq!(ua_main, orchard_ua_from_seed(Network::Main, &seed).unwrap());
        // Parses back as a Zcash address.
        assert!(is_zcash_address(&ua_main));

        // Testnet uses the `utest` HRP and differs from mainnet.
        let ua_test = orchard_ua_from_seed(Network::Test, &seed).unwrap();
        assert!(ua_test.starts_with("utest"), "got {ua_test}");
        assert_ne!(ua_main, ua_test);
    }

    #[test]
    fn rejects_non_zcash_addresses() {
        assert!(!is_zcash_address(
            "0xabc0000000000000000000000000000000000000"
        ));
        assert!(!is_zcash_address("not an address"));
        assert!(!is_zcash_address(""));
    }

    /// The receive address owallet generates must be **Orchard-only**: a
    /// Unified Address carrying an Orchard receiver and nothing else (no
    /// Sapling, no transparent). This decodes the UA and inspects its actual
    /// receivers rather than just the `u1…` prefix (a 3-receiver UA shares it).
    #[test]
    fn generated_ua_is_orchard_only() {
        use zcash_keys::address::Address;

        let seed = bip39_seed_from_stored(ABANDON_12).unwrap();
        for network in [Network::Main, Network::Test] {
            let net: consensus::Network = network.into();
            let ua_str = orchard_ua_from_seed(network, &seed).unwrap();
            let ua = match Address::decode(&net, &ua_str) {
                Some(Address::Unified(ua)) => ua,
                other => panic!("expected a Unified Address, got {other:?} ({ua_str})"),
            };
            assert!(
                ua.orchard().is_some(),
                "{} UA must carry an Orchard receiver",
                network.name()
            );
            assert!(
                ua.sapling().is_none(),
                "{} UA must NOT carry a Sapling receiver",
                network.name()
            );
            assert!(
                ua.transparent().is_none(),
                "{} UA must NOT carry a transparent receiver",
                network.name()
            );
        }
    }
}
