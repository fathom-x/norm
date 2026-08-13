//! ERC-20 USDC transfer.
//!
//! Ports `_send_usdc_async` in `wallet_mcp/server.py:1802`. The flow:
//! 1. parse recipient address + amount (in USDC, not cents)
//! 2. multiply by `10^decimals` to get the raw `uint256`
//! 3. build an ERC-20 `transfer(to, value)` call via `alloy::sol!`
//! 4. sign + send via the JSON-RPC provider; wait for a receipt

use std::str::FromStr;

use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use alloy::sol_types::SolCall;
use owallet_crypto::PrivateKey;

use crate::chains::ChainInfo;
use crate::error::EvmError;

sol! {
    interface IERC20 {
        function transfer(address to, uint256 value) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }
}

#[derive(Debug, Clone)]
pub struct SendUsdcOutcome {
    pub tx_hash: String,
    pub to: String,
    pub amount_raw: String,
    pub amount_human: String,
    pub block_number: Option<u64>,
    pub explorer_url: Option<String>,
}

/// Convert a human-decimal USDC amount (e.g. `1.5`) into the raw `uint256`
/// value used by the ERC-20 contract (e.g. `1_500_000` with 6 decimals).
fn parse_amount(amount: f64, decimals: u8) -> Result<U256, EvmError> {
    if !(amount.is_finite() && amount > 0.0) {
        return Err(EvmError::NonPositiveAmount);
    }
    let scaled = amount * 10f64.powi(decimals as i32);
    let rounded = scaled.round();
    if !(rounded.is_finite() && rounded >= 0.0) || rounded > u128::MAX as f64 {
        return Err(EvmError::AmountOverflow);
    }
    Ok(U256::from(rounded as u128))
}

/// Format a raw uint256 token value back into a human-friendly decimal.
/// Public so the dashboard / CLI / MCP tool can render both ETH (18
/// decimals) and USDC (6 decimals) without duplicating the logic.
pub fn format_amount(raw: U256, decimals: u8) -> String {
    let raw_u128: u128 = raw.try_into().unwrap_or(u128::MAX);
    let div = 10u128.pow(decimals as u32);
    let whole = raw_u128 / div;
    let frac = raw_u128 % div;
    if frac == 0 {
        whole.to_string()
    } else {
        let frac_str = format!("{:0width$}", frac, width = decimals as usize);
        let trimmed = frac_str.trim_end_matches('0');
        format!("{whole}.{trimmed}")
    }
}

/// Sign + broadcast an ERC-20 USDC transfer and wait for the receipt.
pub async fn send_usdc(
    rpc_url: &str,
    chain: &ChainInfo,
    sk: &PrivateKey,
    to: &str,
    amount_usd: f64,
) -> Result<SendUsdcOutcome, EvmError> {
    let to_addr: Address =
        Address::from_str(to).map_err(|_| EvmError::InvalidAddress(to.to_string()))?;
    let usdc: Address = Address::from_str(chain.usdc)
        .map_err(|_| EvmError::InvalidAddress(chain.usdc.to_string()))?;
    let amount_raw = parse_amount(amount_usd, chain.usdc_decimals)?;

    let signer = PrivateKeySigner::from_slice(sk.as_bytes()).map_err(|_| EvmError::InvalidKey)?;
    let from_addr = signer.address();
    let wallet = EthereumWallet::from(signer);
    let url = rpc_url
        .parse()
        .map_err(|e: url::ParseError| EvmError::Transport(format!("rpc url: {e}")))?;
    let provider = ProviderBuilder::new()
        .with_recommended_fillers()
        .wallet(wallet)
        .on_http(url);

    // Encode `transfer(to, value)` calldata.
    let calldata = IERC20::transferCall {
        to: to_addr,
        value: amount_raw,
    }
    .abi_encode();

    let tx = TransactionRequest::default()
        .with_from(from_addr)
        .with_to(usdc)
        .with_input(calldata)
        .with_chain_id(chain.chain_id);

    let pending = provider
        .send_transaction(tx)
        .await
        .map_err(|e| EvmError::Transport(e.to_string()))?;
    let tx_hash = format!("{:#x}", pending.tx_hash());
    let receipt = pending
        .get_receipt()
        .await
        .map_err(|e| EvmError::Transport(e.to_string()))?;

    if !receipt.status() {
        return Err(EvmError::TxFailed(format!("tx {} reverted", tx_hash)));
    }

    let explorer_url = chain.explorer.map(|root| format!("{root}/tx/{tx_hash}"));

    Ok(SendUsdcOutcome {
        tx_hash,
        to: format!("{:#x}", to_addr),
        amount_raw: amount_raw.to_string(),
        amount_human: format_amount(amount_raw, chain.usdc_decimals),
        block_number: receipt.block_number,
        explorer_url,
    })
}

/// Read the native ETH balance for `account` via `rpc_url`. Returns the
/// raw `uint256` wei value; use [`format_amount(_, 18)`] for a human
/// display.
pub async fn eth_balance(rpc_url: &str, account: &str) -> Result<U256, EvmError> {
    let account: Address =
        Address::from_str(account).map_err(|_| EvmError::InvalidAddress(account.to_string()))?;
    let url = rpc_url
        .parse()
        .map_err(|e: url::ParseError| EvmError::Transport(format!("rpc url: {e}")))?;
    let provider = ProviderBuilder::new().on_http(url);
    provider
        .get_balance(account)
        .await
        .map_err(|e| EvmError::Transport(e.to_string()))
}

/// Read the USDC balance for `account` on `chain` via `rpc_url`. Returns
/// the raw `uint256` value; use [`format_amount`] for a human display.
pub async fn usdc_balance(
    rpc_url: &str,
    chain: &ChainInfo,
    account: &str,
) -> Result<U256, EvmError> {
    use alloy::sol_types::SolValue;

    let account: Address =
        Address::from_str(account).map_err(|_| EvmError::InvalidAddress(account.to_string()))?;
    let usdc: Address = Address::from_str(chain.usdc)
        .map_err(|_| EvmError::InvalidAddress(chain.usdc.to_string()))?;

    let url = rpc_url
        .parse()
        .map_err(|e: url::ParseError| EvmError::Transport(format!("rpc url: {e}")))?;
    let provider = ProviderBuilder::new().on_http(url);

    let calldata = IERC20::balanceOfCall { account }.abi_encode();
    let tx = TransactionRequest::default()
        .with_to(usdc)
        .with_input(calldata);
    let raw = provider
        .call(&tx)
        .await
        .map_err(|e| EvmError::Transport(e.to_string()))?;
    let decoded = U256::abi_decode(raw.as_ref(), true)
        .map_err(|e| EvmError::Transport(format!("decode balanceOf: {e}")))?;
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_amount_round_trips_basic_values() {
        let v = parse_amount(1.0, 6).unwrap();
        assert_eq!(v, U256::from(1_000_000u64));
        let v = parse_amount(0.01, 6).unwrap();
        assert_eq!(v, U256::from(10_000u64));
    }

    #[test]
    fn parse_amount_rejects_non_positive() {
        assert!(matches!(
            parse_amount(0.0, 6),
            Err(EvmError::NonPositiveAmount)
        ));
        assert!(matches!(
            parse_amount(-1.0, 6),
            Err(EvmError::NonPositiveAmount)
        ));
        assert!(matches!(
            parse_amount(f64::NAN, 6),
            Err(EvmError::NonPositiveAmount)
        ));
    }

    #[test]
    fn format_amount_matches_examples() {
        assert_eq!(format_amount(U256::from(1_000_000u64), 6), "1");
        assert_eq!(format_amount(U256::from(1_500_000u64), 6), "1.5");
        assert_eq!(format_amount(U256::from(123u64), 6), "0.000123");
        assert_eq!(format_amount(U256::from(0u64), 6), "0");
    }
}
