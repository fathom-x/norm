//! Chain metadata: USDC contract address + decimals per chain.
//!
//! Replaces the `x402.mechanisms.evm.utils.get_asset_info` dependency from
//! the Python implementation (which was only used to look up these two
//! pieces of data per chain). Hard-coding it removes the entire x402
//! transitive dep without losing functionality. Matches the table in
//! `wallet_mcp/server.py:58-90`.

use crate::error::EvmError;

#[derive(Debug, Clone)]
pub struct ChainInfo {
    pub chain_id: u64,
    pub name: &'static str,
    pub usdc: &'static str,
    pub usdc_decimals: u8,
    pub explorer: Option<&'static str>,
}

/// Look up a chain by CAIP-2 id (e.g. `eip155:8453`).
pub fn from_caip2(caip2: &str) -> Result<ChainInfo, EvmError> {
    let chain_id: u64 = caip2
        .strip_prefix("eip155:")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| EvmError::UnsupportedChain(caip2.to_string()))?;
    from_chain_id(chain_id)
}

pub fn from_chain_id(chain_id: u64) -> Result<ChainInfo, EvmError> {
    Ok(match chain_id {
        1 => ChainInfo {
            chain_id: 1,
            name: "Ethereum",
            usdc: "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            usdc_decimals: 6,
            explorer: Some("https://etherscan.io"),
        },
        8453 => ChainInfo {
            chain_id: 8453,
            name: "Base",
            usdc: "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
            usdc_decimals: 6,
            explorer: Some("https://basescan.org"),
        },
        84532 => ChainInfo {
            chain_id: 84532,
            name: "Base Sepolia",
            usdc: "0x036cbd53842c5426634e7929541ec2318f3dcf7e",
            usdc_decimals: 6,
            explorer: Some("https://sepolia.basescan.org"),
        },
        10 => ChainInfo {
            chain_id: 10,
            name: "Optimism",
            usdc: "0x0b2c639c533813f4aa9d7837caf62653d097ff85",
            usdc_decimals: 6,
            explorer: Some("https://optimistic.etherscan.io"),
        },
        137 => ChainInfo {
            chain_id: 137,
            name: "Polygon",
            usdc: "0x3c499c542cef5e3811e1192ce70d8cc03d5c3359",
            usdc_decimals: 6,
            explorer: Some("https://polygonscan.com"),
        },
        42161 => ChainInfo {
            chain_id: 42161,
            name: "Arbitrum One",
            usdc: "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
            usdc_decimals: 6,
            explorer: Some("https://arbiscan.io"),
        },
        other => return Err(EvmError::UnsupportedChain(format!("eip155:{other}"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_mainnet_resolves() {
        let info = from_caip2("eip155:8453").unwrap();
        assert_eq!(info.chain_id, 8453);
        assert_eq!(info.name, "Base");
        assert_eq!(info.usdc_decimals, 6);
    }

    #[test]
    fn unknown_chain_errors() {
        assert!(matches!(
            from_caip2("eip155:99999"),
            Err(EvmError::UnsupportedChain(_))
        ));
        assert!(from_caip2("solana:foo").is_err());
    }
}
