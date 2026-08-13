//! Zcash network selection. A thin wrapper over
//! `zcash_protocol::consensus::Network`, mirroring zkv's `data::Network`.

use zcash_protocol::consensus;

use crate::error::ZcashError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Network {
    #[default]
    Main,
    Test,
}

impl Network {
    /// Parse `"mainnet"`/`"main"` or `"testnet"`/`"test"`.
    pub fn parse(name: &str) -> Result<Network, ZcashError> {
        match name.trim().to_ascii_lowercase().as_str() {
            "mainnet" | "main" => Ok(Network::Main),
            "testnet" | "test" => Ok(Network::Test),
            _ => Err(ZcashError::InvalidNetwork(name.to_string())),
        }
    }

    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Network::Main => "mainnet",
            Network::Test => "testnet",
        }
    }

    /// Currency ticker for display (mainnet ZEC vs testnet TAZ).
    #[must_use]
    pub fn ticker(&self) -> &'static str {
        match self {
            Network::Main => "ZEC",
            Network::Test => "TAZ",
        }
    }
}

impl From<Network> for consensus::Network {
    fn from(value: Network) -> Self {
        match value {
            Network::Main => consensus::Network::MainNetwork,
            Network::Test => consensus::Network::TestNetwork,
        }
    }
}

impl From<consensus::Network> for Network {
    fn from(value: consensus::Network) -> Self {
        match value {
            consensus::Network::MainNetwork => Network::Main,
            consensus::Network::TestNetwork => Network::Test,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aliases() {
        assert_eq!(Network::parse("mainnet").unwrap(), Network::Main);
        assert_eq!(Network::parse("MAIN").unwrap(), Network::Main);
        assert_eq!(Network::parse("testnet").unwrap(), Network::Test);
        assert_eq!(Network::parse(" test ").unwrap(), Network::Test);
        assert!(Network::parse("regtest").is_err());
    }

    #[test]
    fn ticker_matches_network() {
        assert_eq!(Network::Main.ticker(), "ZEC");
        assert_eq!(Network::Test.ticker(), "TAZ");
    }
}
