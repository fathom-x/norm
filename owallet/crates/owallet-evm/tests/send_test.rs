//! End-to-end tests for `send_usdc` against a wiremock JSON-RPC.
//!
//! wiremock pretends to be an Ethereum RPC endpoint and replies with the
//! canned responses an actual node would produce.

use owallet_crypto::{derive_from_mnemonic, Mnemonic, EVM_HD_PATH};
use owallet_evm::{chains, send_usdc};
use serde_json::{json, Value};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const ABANDON_12: &str = "abandon abandon abandon abandon abandon abandon \
     abandon abandon abandon abandon abandon about";

/// Canned JSON-RPC responses for the methods alloy fires during a
/// `sendTransaction → getTransactionReceipt` cycle. Hardcoded to Base
/// mainnet (`chain_id=0x2105=8453`).
///
/// Kept in lockstep with the copy in `owallet-http/tests/mcp_test.rs`
/// (see the `JsonRpcMock` there). The copies are deliberately
/// duplicated — extracting into a shared `pub mod test_support`
/// behind a feature gate broke `cargo test --workspace` (the gated
/// integration test silently skipped).
struct JsonRpcMock;

impl Respond for JsonRpcMock {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let method = body["method"].as_str().unwrap_or("");
        let id = body["id"].clone();

        let result: Value = match method {
            "eth_chainId" => json!("0x2105"), // Base mainnet (8453)
            "eth_getTransactionCount" => json!("0x0"),
            "eth_getBalance" => json!("0xde0b6b3a7640000"), // 1 ETH
            "eth_gasPrice" => json!("0x3b9aca00"),          // 1 gwei
            "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
            "eth_feeHistory" => json!({
                "baseFeePerGas": ["0x1", "0x1"],
                "gasUsedRatio": [0.5_f64],
                "oldestBlock": "0x0",
                "reward": [["0x3b9aca00"]],
            }),
            "eth_estimateGas" => json!("0xea60"), // 60_000 gas
            "eth_blockNumber" => json!("0x1"),
            "eth_getBlockByNumber" => json!({
                "number": "0x1",
                "baseFeePerGas": "0x1",
            }),
            "eth_sendRawTransaction" => {
                json!("0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a89e9ae37e9b97c4a4e0e0e0e0e0e0e0e")
            }
            "eth_getTransactionReceipt" => json!({
                "transactionHash":
                    "0x6d6b56b3acba7ebe1e44b8a5b1bb9d8a89e9ae37e9b97c4a4e0e0e0e0e0e0e0e",
                "transactionIndex": "0x0",
                "blockHash":
                    "0x0000000000000000000000000000000000000000000000000000000000000001",
                "blockNumber": "0x1234",
                "from": "0x9858effd232b4033e47d90003d41ec34ecaeda94",
                "to": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                "cumulativeGasUsed": "0xea60",
                "gasUsed": "0xea60",
                "contractAddress": null,
                "logs": [],
                "logsBloom": format!("0x{}", "0".repeat(512)),
                "type": "0x2",
                "effectiveGasPrice": "0x3b9aca00",
                "status": "0x1",
            }),
            _ => json!(null),
        };

        ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_usdc_signs_and_broadcasts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(JsonRpcMock)
        .mount(&server)
        .await;

    let m = Mnemonic::parse(ABANDON_12).unwrap();
    let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
    let base = chains::from_chain_id(8453).unwrap();

    let outcome = send_usdc(
        &server.uri(),
        &base,
        &sk,
        "0x000000000000000000000000000000000000dead",
        1.25,
    )
    .await
    .expect("send_usdc");
    assert!(outcome.tx_hash.starts_with("0x"));
    assert_eq!(outcome.amount_human, "1.25");
    assert_eq!(outcome.amount_raw, "1250000");
    assert!(outcome
        .explorer_url
        .as_deref()
        .unwrap()
        .starts_with("https://basescan.org/tx/"));
    assert_eq!(outcome.block_number, Some(0x1234));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_usdc_rejects_zero_amount() {
    // No mock needed; the amount is rejected before any RPC fires.
    let m = Mnemonic::parse(ABANDON_12).unwrap();
    let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
    let base = chains::from_chain_id(8453).unwrap();
    let err = send_usdc(
        "http://127.0.0.1:1",
        &base,
        &sk,
        "0x000000000000000000000000000000000000dead",
        0.0,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, owallet_evm::EvmError::NonPositiveAmount));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_usdc_rejects_bad_recipient() {
    let m = Mnemonic::parse(ABANDON_12).unwrap();
    let sk = derive_from_mnemonic(&m, EVM_HD_PATH).unwrap();
    let base = chains::from_chain_id(8453).unwrap();
    let err = send_usdc("http://127.0.0.1:1", &base, &sk, "not an address", 1.0)
        .await
        .unwrap_err();
    assert!(matches!(err, owallet_evm::EvmError::InvalidAddress(_)));
}
