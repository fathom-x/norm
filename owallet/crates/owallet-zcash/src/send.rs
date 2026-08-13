//! Build, prove, sign, and broadcast a shielded Orchard payment. Adapted from
//! zkv's `internal/send.rs::pay`, generalized from a zero-value memo write to a
//! real value transfer to an external Unified Address.

use std::num::NonZeroUsize;
use std::str::FromStr;

use anyhow::anyhow;

use zcash_address::ZcashAddress;
use zcash_client_backend::{
    data_api::{
        error::Error as WalletError,
        wallet::{
            create_proposed_transactions, input_selection::GreedyInputSelector,
            input_selection::GreedyInputSelectorError, propose_transfer, ConfirmationsPolicy,
            SpendingKeys,
        },
        Account, WalletRead,
    },
    fees::{standard::MultiOutputChangeStrategy, DustOutputPolicy, SplitPolicy, StandardFeeRule},
    proto::service,
    wallet::OvkPolicy,
};
use zcash_client_sqlite::{error::SqliteClientError, wallet::commitment_tree, ReceivedNoteId};
use zcash_primitives::transaction::fees::zip317;
use zcash_proofs::prover::LocalTxProver;
use zcash_protocol::{value::Zatoshis, ShieldedProtocol};
use zip321::{Payment, TransactionRequest};

/// Concrete error type returned by `propose_transfer` / `create_proposed_transactions`
/// for our wallet, so the `.map_err` closures aren't left with an unconstrained
/// commitment-tree error parameter (mirrors zkv's `WalletErrorT`).
type WalletErrorT = WalletError<
    SqliteClientError,
    commitment_tree::Error,
    GreedyInputSelectorError,
    zip317::FeeError,
    zip317::FeeError,
    ReceivedNoteId,
>;

use crate::{
    amount::{format_zec, parse_zec_to_zat},
    db::open_wallet_db,
    error::ZcashError,
    keys::spending_key,
    network::Network,
    remote,
};

/// Note-management defaults — single recipient per tx (mirrors zkv).
const TARGET_NOTE_COUNT: usize = 4;
const MIN_SPLIT_OUTPUT_VALUE: u64 = 10_000_000;

/// Result of a successful broadcast.
#[derive(Debug, Clone)]
pub struct SendZcashOutcome {
    pub txid: String,
    pub to: String,
    pub amount_zat: String,
    pub amount_human: String,
    /// Additional txids when the proposal legitimately split into several
    /// transactions (the primary is `txid`).
    pub other_txids: Vec<String>,
}

/// Send `amount_zec` to a Unified Address. The wallet must already be synced so
/// it has spendable notes; callers run [`crate::sync::sync`] first.
pub async fn send_zcash(
    dir: &std::path::Path,
    network: Network,
    lightwalletd: &str,
    seed: &[u8; 64],
    to_ua: &str,
    amount_zec: f64,
) -> Result<SendZcashOutcome, ZcashError> {
    let zat = parse_zec_to_zat(amount_zec)?;
    if !crate::keys::is_zcash_address(to_ua) {
        return Err(ZcashError::InvalidAddress(to_ua.to_string()));
    }
    send_inner(dir, network, lightwalletd, seed, to_ua, zat)
        .await
        .map_err(|e| ZcashError::Backend(format!("{e:#}")))
}

async fn send_inner(
    dir: &std::path::Path,
    network: Network,
    lightwalletd: &str,
    seed: &[u8; 64],
    to_ua: &str,
    zat: u64,
) -> anyhow::Result<SendZcashOutcome> {
    let params: zcash_protocol::consensus::Network = network.into();
    let mut db_data = open_wallet_db(dir, network)?;

    let account_ids = db_data.get_account_ids()?;
    let account_id = match account_ids.as_slice() {
        [id] => *id,
        [] => anyhow::bail!("wallet has no Zcash account; run sync first"),
        _ => anyhow::bail!("wallet has multiple Zcash accounts; expected one"),
    };
    let account = db_data
        .get_account(account_id)?
        .ok_or_else(|| anyhow!("account vanished"))?;
    account
        .source()
        .key_derivation()
        .ok_or_else(|| anyhow!("cannot spend from a watch-only account"))?;

    let usk = spending_key(network, seed)?;

    // Build the ZIP-321 payment request: a value output to the UA, no memo.
    let recipient = ZcashAddress::from_str(to_ua).map_err(|e| anyhow!("bad recipient UA: {e}"))?;
    let payment = Payment::new(
        recipient,
        Some(Zatoshis::from_u64(zat)?),
        None,
        None,
        None,
        vec![],
    )
    .map_err(|e| anyhow!("failed to build payment: {e}"))?;
    let request =
        TransactionRequest::new(vec![payment]).map_err(|e| anyhow!("bad request: {e}"))?;

    let mut client = remote::connect(lightwalletd, network).await?;

    let prover = LocalTxProver::bundled();
    let change_strategy = MultiOutputChangeStrategy::new(
        StandardFeeRule::Zip317,
        None,
        ShieldedProtocol::Orchard,
        DustOutputPolicy::default(),
        SplitPolicy::with_min_output_value(
            NonZeroUsize::new(TARGET_NOTE_COUNT).expect("nonzero const"),
            Zatoshis::from_u64(MIN_SPLIT_OUTPUT_VALUE)?,
        ),
    );
    let input_selector = GreedyInputSelector::new();

    let proposal = propose_transfer(
        &mut db_data,
        &params,
        account.id(),
        &input_selector,
        &change_strategy,
        request,
        ConfirmationsPolicy::default(),
        None,
    )
    .map_err(|e: WalletErrorT| anyhow!("propose transfer: {e:?}"))?;

    let txids = create_proposed_transactions(
        &mut db_data,
        &params,
        &prover,
        &prover,
        &SpendingKeys::from_unified_spending_key(usk),
        OvkPolicy::Sender,
        &proposal,
        None,
    )
    .map_err(|e: WalletErrorT| anyhow!("create transactions: {e:?}"))?;

    // Broadcast every transaction the proposal produced, in order.
    let mut broadcast = Vec::new();
    for txid in txids.iter() {
        let (txid, raw_tx) = db_data
            .get_transaction(*txid)?
            .map(|tx| {
                let mut raw_tx = service::RawTransaction::default();
                tx.write(&mut raw_tx.data).unwrap();
                (tx.txid(), raw_tx)
            })
            .ok_or_else(|| anyhow!("transaction {txid:?} not found after creation"))?;
        let response = client
            .send_transaction(raw_tx)
            .await
            .map_err(|e| anyhow!("broadcast: {e}"))?
            .into_inner();
        if response.error_code != 0 {
            return Err(ZcashError::SendFailed {
                code: response.error_code,
                reason: response.error_message,
            }
            .into());
        }
        broadcast.push(txid.to_string());
    }

    let primary = broadcast
        .last()
        .cloned()
        .ok_or_else(|| anyhow!("proposal produced no transactions"))?;
    let other_txids = broadcast[..broadcast.len() - 1].to_vec();

    Ok(SendZcashOutcome {
        txid: primary,
        to: to_ua.to_string(),
        amount_zat: zat.to_string(),
        amount_human: format_zec(zat),
        other_txids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The amount + recipient guards short-circuit before any lightwalletd
    // call, so they're testable without a network. (A valid spend itself
    // needs a funded, synced wallet + live lightwalletd and is exercised
    // manually / outside the sandbox.)
    #[test]
    fn send_rejects_bad_inputs_without_network() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let seed = [4u8; 64];
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        // Non-positive amount is rejected before the address is even checked.
        let err = rt.block_on(send_zcash(
            dir,
            Network::Main,
            "zecrocks",
            &seed,
            "u1anything",
            0.0,
        ));
        assert!(matches!(err, Err(ZcashError::NonPositiveAmount)), "{err:?}");

        // A non-Zcash recipient (e.g. an EVM address) is rejected.
        let err = rt.block_on(send_zcash(
            dir,
            Network::Main,
            "zecrocks",
            &seed,
            "0xabc0000000000000000000000000000000000000",
            1.0,
        ));
        assert!(matches!(err, Err(ZcashError::InvalidAddress(_))), "{err:?}");
    }
}
