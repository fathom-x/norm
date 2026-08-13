//! Wallet sync: scan compact blocks from lightwalletd, then enhance (fetch full
//! txs so memos/values land). Adapted from zkv's `internal/sync.rs`, trimmed to
//! the Orchard-only path: no transparent-UTXO refresh, no mempool, no
//! `pending.toml` GC, and no interactive wipe prompt. Block deletion is inline
//! (no `tokio::spawn`) so the CLI's `block_on` runtime flavor doesn't matter.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::anyhow;
use futures_util::TryStreamExt;
use orchard::tree::MerkleHashOrchard;
use prost::Message;
use tonic::{transport::Channel, Code};
use tracing::{debug, info};

use zcash_client_backend::{
    data_api::{
        chain::{
            error::Error as ChainError, scan_cached_blocks, BlockSource, ChainState,
            CommitmentTreeRoot,
        },
        scanning::{ScanPriority, ScanRange},
        wallet::decrypt_and_store_transaction,
        TransactionDataRequest, TransactionStatus, WalletCommitmentTrees, WalletRead, WalletWrite,
    },
    proto::service::{
        self, compact_tx_streamer_client::CompactTxStreamerClient, BlockId, RawTransaction,
    },
};
use zcash_client_sqlite::{chain::BlockMeta, error::SqliteClientError, FsBlockDb, FsBlockDbError};
use zcash_primitives::{
    merkle_tree::HashSer,
    transaction::{Transaction, TxId},
};
use zcash_protocol::consensus::{self, BlockHeight, BranchId};

use crate::{
    data::block_path,
    db::{open_raw_readonly, open_wallet_db, ZWalletDb},
    error::ZcashError,
    network::Network,
    remote,
};

const BATCH_SIZE: u32 = 10_000;

/// Sync the wallet to chain tip and decrypt received transactions. Returns the
/// synced chain height.
pub async fn sync(dir: &Path, network: Network, lightwalletd: &str) -> Result<u32, ZcashError> {
    run_sync_inner(dir, network, lightwalletd)
        .await
        .map_err(|e| ZcashError::Backend(format!("{e:#}")))
}

async fn run_sync_inner(dir: &Path, network: Network, lightwalletd: &str) -> anyhow::Result<u32> {
    let params: consensus::Network = network.into();

    let mut db_cache = FsBlockDb::for_path(dir).map_err(|e| anyhow!("open block cache: {e:?}"))?;
    let mut db_data = open_wallet_db(dir, network)?;
    let mut client = remote::connect(lightwalletd, network).await?;

    // Fast path: nothing to do if the tip is unchanged and no scan is pending.
    let rpc_tip: u32 = client
        .get_latest_block(service::ChainSpec::default())
        .await?
        .get_ref()
        .height
        .try_into()
        .map_err(|_| anyhow!("chain tip out of range"))?;
    let wallet_tip = db_data.chain_height()?.map(u32::from);
    let pending_scan = !db_data.suggest_scan_ranges()?.is_empty();
    info!("Tip check: rpc={rpc_tip} wallet={wallet_tip:?} pending_scan={pending_scan}");
    if wallet_tip == Some(rpc_tip) && !pending_scan {
        return Ok(rpc_tip);
    }

    update_subtree_roots(&mut client, &mut db_data).await?;

    while sync_pass(&mut client, &params, dir, &mut db_cache, &mut db_data).await? {}

    info!("fetching full transactions to decrypt memos");
    enhance(&mut client, &params, &mut db_data).await?;

    Ok(db_data.chain_height()?.map(u32::from).unwrap_or(0))
}

/// One pass: download blocks, scan, repeat as suggested. Returns `true` if it
/// should loop again (chain tip moved or reorg).
async fn sync_pass(
    client: &mut CompactTxStreamerClient<Channel>,
    params: &consensus::Network,
    dir: &Path,
    db_cache: &mut FsBlockDb,
    db_data: &mut ZWalletDb,
) -> anyhow::Result<bool> {
    let _chain_tip = update_chain_tip(client, db_data).await?;

    let mut scan_ranges = db_data.suggest_scan_ranges()?;
    info!("fetched {} scan ranges", scan_ranges.len());

    // Verify any pending range first.
    loop {
        match scan_ranges.first() {
            Some(scan_range) if scan_range.priority() == ScanPriority::Verify => {
                let block_meta = download_blocks(client, dir, db_cache, scan_range).await?;
                let chain_state =
                    download_chain_state(client, scan_range.block_range().start - 1).await?;
                let updated =
                    scan_blocks(params, dir, db_cache, db_data, &chain_state, scan_range)?;
                delete_cached_blocks(dir, block_meta);
                if updated {
                    scan_ranges = db_data.suggest_scan_ranges()?;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }

    let scan_ranges = db_data.suggest_scan_ranges()?;
    debug!("Suggested ranges: {:?}", scan_ranges);

    for scan_range in scan_ranges.into_iter().flat_map(|r| {
        (0..).scan(r, |acc, _| {
            if acc.is_empty() {
                None
            } else if let Some((cur, next)) = acc.split_at(acc.block_range().start + BATCH_SIZE) {
                *acc = next;
                Some(cur)
            } else {
                let cur = acc.clone();
                let end = acc.block_range().end;
                *acc = ScanRange::from_parts(end..end, acc.priority());
                Some(cur)
            }
        })
    }) {
        let block_meta = download_blocks(client, dir, db_cache, &scan_range).await?;
        let chain_state = download_chain_state(client, scan_range.block_range().start - 1).await?;
        let updated = scan_blocks(params, dir, db_cache, db_data, &chain_state, &scan_range)?;
        delete_cached_blocks(dir, block_meta);
        if updated {
            return Ok(true);
        }
    }

    Ok(false)
}

async fn update_subtree_roots(
    client: &mut CompactTxStreamerClient<Channel>,
    db_data: &mut ZWalletDb,
) -> anyhow::Result<()> {
    let mut request = service::GetSubtreeRootsArg::default();
    request.set_shielded_protocol(service::ShieldedProtocol::Sapling);
    let sapling_roots: Vec<CommitmentTreeRoot<sapling::Node>> = client
        .get_subtree_roots(request)
        .await?
        .into_inner()
        .and_then(|root| async move {
            let root_hash = sapling::Node::read(&root.root_hash[..])?;
            Ok(CommitmentTreeRoot::from_parts(
                BlockHeight::from_u32(root.completing_block_height as u32),
                root_hash,
            ))
        })
        .try_collect()
        .await?;
    info!("Sapling tree: {} subtrees", sapling_roots.len());
    db_data.put_sapling_subtree_roots(0, &sapling_roots)?;

    let mut request = service::GetSubtreeRootsArg::default();
    request.set_shielded_protocol(service::ShieldedProtocol::Orchard);
    let orchard_roots: Vec<CommitmentTreeRoot<MerkleHashOrchard>> = client
        .get_subtree_roots(request)
        .await?
        .into_inner()
        .and_then(|root| async move {
            let root_hash = MerkleHashOrchard::read(&root.root_hash[..])?;
            Ok(CommitmentTreeRoot::from_parts(
                BlockHeight::from_u32(root.completing_block_height as u32),
                root_hash,
            ))
        })
        .try_collect()
        .await?;
    info!("Orchard tree: {} subtrees", orchard_roots.len());
    db_data.put_orchard_subtree_roots(0, &orchard_roots)?;

    Ok(())
}

async fn update_chain_tip(
    client: &mut CompactTxStreamerClient<Channel>,
    db_data: &mut ZWalletDb,
) -> anyhow::Result<BlockHeight> {
    let tip_height: BlockHeight = client
        .get_latest_block(service::ChainSpec::default())
        .await?
        .get_ref()
        .height
        .try_into()
        .map_err(|_| anyhow!("chain tip out of range"))?;
    info!("Chain tip: {}", tip_height);
    db_data.update_chain_tip(tip_height)?;
    Ok(tip_height)
}

async fn download_blocks(
    client: &mut CompactTxStreamerClient<Channel>,
    dir: &Path,
    db_cache: &FsBlockDb,
    scan_range: &ScanRange,
) -> anyhow::Result<Vec<BlockMeta>> {
    info!("Fetching {}", scan_range);
    let mut start = service::BlockId::default();
    start.height = scan_range.block_range().start.into();
    let mut end = service::BlockId::default();
    end.height = (scan_range.block_range().end - 1).into();
    let range = service::BlockRange {
        start: Some(start),
        end: Some(end),
        pool_types: Default::default(),
    };
    let dir = dir.to_owned();
    let stream = client
        .get_block_range(range)
        .await?
        .into_inner()
        .and_then(move |block| {
            let dir = dir.clone();
            async move {
                let (sapling_outputs_count, orchard_actions_count) = block
                    .vtx
                    .iter()
                    .map(|tx| (tx.outputs.len() as u32, tx.actions.len() as u32))
                    .fold((0, 0), |(s, o), (sn, on)| (s + sn, o + on));
                let meta = BlockMeta {
                    height: block.height(),
                    block_hash: block.hash(),
                    block_time: block.time,
                    sapling_outputs_count,
                    orchard_actions_count,
                };
                let encoded = block.encode_to_vec();
                let path = block_path(&dir, &meta);
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                let mut f = tokio::fs::File::create(&path).await?;
                tokio::io::AsyncWriteExt::write_all(&mut f, &encoded).await?;
                Ok(meta)
            }
        });
    tokio::pin!(stream);

    let mut block_meta = vec![];
    while let Some(block) = stream.try_next().await? {
        block_meta.push(block);
    }
    db_cache
        .write_block_metadata(&block_meta)
        .map_err(|e| anyhow!("write block metadata: {e:?}"))?;
    Ok(block_meta)
}

async fn download_chain_state(
    client: &mut CompactTxStreamerClient<Channel>,
    block_height: BlockHeight,
) -> anyhow::Result<ChainState> {
    let tree_state = client
        .get_tree_state(BlockId {
            height: block_height.into(),
            hash: vec![],
        })
        .await?;
    Ok(tree_state.into_inner().to_chain_state()?)
}

/// Delete cached compact-block files for a scanned range. Inline (synchronous)
/// so we don't depend on a multi-thread tokio runtime.
fn delete_cached_blocks(dir: &Path, block_meta: Vec<BlockMeta>) {
    for meta in block_meta {
        let path = block_path(dir, &meta);
        if let Err(e) = std::fs::remove_file(&path) {
            tracing::warn!("Failed to remove {:?}: {}", path, e);
        }
    }
}

/// Reorg recovery exhausted: no valid rewind target for the conflict point.
#[derive(Debug)]
pub struct UnrecoverableRewind {
    pub at_height: BlockHeight,
    pub requested: BlockHeight,
}

impl std::fmt::Display for UnrecoverableRewind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Chain reorg at height {} could not be recovered (requested rewind to {}); \
             delete the wallet data directory and resync.",
            self.at_height, self.requested
        )
    }
}

impl std::error::Error for UnrecoverableRewind {}

/// Highest height in `blocks` that also has shared sapling+orchard checkpoints,
/// bounded by `max_height`.
fn find_shallow_rewind_target(
    dir: &Path,
    max_height: BlockHeight,
) -> anyhow::Result<Option<BlockHeight>> {
    use rusqlite::OptionalExtension;
    let conn = open_raw_readonly(dir)?;
    let h: Option<u32> = conn
        .query_row(
            "SELECT MAX(blocks.height) FROM blocks
             JOIN sapling_tree_checkpoints sc ON sc.checkpoint_id = blocks.height
             JOIN orchard_tree_checkpoints oc ON oc.checkpoint_id = blocks.height
             WHERE blocks.height <= ?1",
            [u32::from(max_height)],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(h.map(BlockHeight::from))
}

fn perform_rewind(
    db_data: &mut ZWalletDb,
    dir: &Path,
    at_height: BlockHeight,
    requested: BlockHeight,
) -> anyhow::Result<BlockHeight> {
    match db_data.truncate_to_height(requested) {
        Ok(h) => Ok(h),
        Err(SqliteClientError::RequestedRewindInvalid {
            safe_rewind_height, ..
        }) => {
            if let Some(safe) = safe_rewind_height.filter(|&s| s < requested) {
                info!("No checkpoint at {requested}; trying safe rewind to {safe}");
                if let Ok(h) = db_data.truncate_to_height(safe) {
                    return Ok(h);
                }
            }
            if let Some(target) = find_shallow_rewind_target(dir, at_height)? {
                info!("Shallow rewind to {target}");
                return db_data
                    .truncate_to_height(target)
                    .map_err(|e| anyhow!("{:?}", e));
            }
            Err(UnrecoverableRewind {
                at_height,
                requested,
            }
            .into())
        }
        Err(e) => Err(anyhow!("{:?}", e)),
    }
}

fn scan_blocks(
    params: &consensus::Network,
    dir: &Path,
    db_cache: &mut FsBlockDb,
    db_data: &mut ZWalletDb,
    initial_chain_state: &ChainState,
    scan_range: &ScanRange,
) -> anyhow::Result<bool> {
    info!("Scanning {}", scan_range);
    let scan_result = scan_cached_blocks(
        params,
        db_cache,
        db_data,
        scan_range.block_range().start,
        initial_chain_state,
        scan_range.len(),
    );

    match scan_result {
        Err(ChainError::Scan(err)) if err.is_continuity_error() => {
            let requested = err.at_height().saturating_sub(10);
            info!(
                "Chain reorg at {}, rewinding to {}",
                err.at_height(),
                requested
            );
            let rewind_height = perform_rewind(db_data, dir, err.at_height(), requested)?;
            db_cache
                .with_blocks(Some(rewind_height + 1), None, |block| {
                    let meta = BlockMeta {
                        height: block.height(),
                        block_hash: block.hash(),
                        block_time: block.time,
                        sapling_outputs_count: 0,
                        orchard_actions_count: 0,
                    };
                    std::fs::remove_file(block_path(dir, &meta))
                        .map_err(|e| ChainError::<(), _>::BlockSource(FsBlockDbError::Fs(e)))
                })
                .map_err(|e| anyhow!("{:?}", e))?;
            db_cache
                .truncate_to_height(rewind_height)
                .map_err(|e| anyhow!("{:?}", e))?;
            Ok(true)
        }
        Ok(_) => {
            let latest_ranges = db_data.suggest_scan_ranges()?;
            Ok(if let Some(range) = latest_ranges.first() {
                range.priority() > scan_range.priority()
            } else {
                false
            })
        }
        Err(e) => Err(anyhow!("{:?}", e)),
    }
}

// ---- Enhance pass ----

fn parse_raw_transaction(
    params: &consensus::Network,
    chain_tip: BlockHeight,
    tx: RawTransaction,
) -> anyhow::Result<(Transaction, Option<BlockHeight>)> {
    let mined_height = (tx.height > 0 && tx.height <= u64::from(u32::MAX))
        .then(|| BlockHeight::from_u32(u32::try_from(tx.height).unwrap()));
    let tx = Transaction::read(
        &tx.data[..],
        BranchId::for_height(params, mined_height.unwrap_or(chain_tip)),
    )?;
    Ok((tx, mined_height))
}

async fn fetch_transaction(
    client: &mut CompactTxStreamerClient<Channel>,
    params: &consensus::Network,
    chain_tip: BlockHeight,
    txid: TxId,
) -> anyhow::Result<Option<(Transaction, Option<BlockHeight>)>> {
    let request = service::TxFilter {
        hash: txid.as_ref().to_vec(),
        ..Default::default()
    };
    let raw_tx = match client.get_transaction(request).await {
        Ok(response) => Ok(Some(response.into_inner())),
        Err(status) => {
            if status.code() == Code::NotFound {
                Ok(None)
            } else {
                Err(status)
            }
        }
    }?;
    raw_tx
        .map(|raw_tx| parse_raw_transaction(params, chain_tip, raw_tx))
        .transpose()
}

async fn enhance(
    client: &mut CompactTxStreamerClient<Channel>,
    params: &consensus::Network,
    db_data: &mut ZWalletDb,
) -> anyhow::Result<()> {
    let chain_tip = match db_data.chain_height()? {
        Some(h) => h,
        None => return Ok(()),
    };

    let mut satisfied = BTreeSet::new();
    loop {
        let mut any_new = false;
        for req in db_data.transaction_data_requests()? {
            if satisfied.contains(&req) {
                continue;
            }
            any_new = true;
            match &req {
                TransactionDataRequest::GetStatus(txid) => {
                    let status = fetch_transaction(client, params, chain_tip, *txid)
                        .await?
                        .map_or(TransactionStatus::TxidNotRecognized, |(_, mined)| {
                            mined
                                .map_or(TransactionStatus::NotInMainChain, TransactionStatus::Mined)
                        });
                    db_data.set_transaction_status(*txid, status)?;
                }
                TransactionDataRequest::Enhancement(txid) => {
                    match fetch_transaction(client, params, chain_tip, *txid).await? {
                        None => db_data
                            .set_transaction_status(*txid, TransactionStatus::TxidNotRecognized)?,
                        Some((tx, mined)) => {
                            decrypt_and_store_transaction(params, db_data, &tx, mined)?
                        }
                    }
                }
                // `TransactionsInvolvingAddress` only exists with the
                // `transparent-inputs` feature, which this Orchard-only build
                // does not enable, so the enum has no such variant here.
                #[allow(unreachable_patterns)]
                _ => {}
            }
            satisfied.insert(req);
        }
        if !any_new {
            break;
        }
    }
    Ok(())
}
