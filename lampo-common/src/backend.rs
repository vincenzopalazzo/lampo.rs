//! ...
//! Beckend implementation
use std::sync::Arc;

use bitcoin::absolute::Height;
use bitcoin::block::Header as BlockHeader;

use async_trait::async_trait;
pub use bitcoin::consensus::{deserialize, serialize};
pub use bitcoin::{Block, BlockHash, Script, Transaction, Txid};
pub use lightning::chain::WatchedOutput;
pub use lightning::routing::utxo::UtxoResult;
pub use lightning_block_sync::{BlockData, BlockHeaderData, BlockSourceResult};
use serde::{Deserialize, Serialize};

use crate::error;
use crate::handler::Handler;
use crate::types::{LampoChainMonitor, LampoChannel, LampoMonitorListener, LampoSweeper};

pub use lightning::chain::BlockLocator;

#[derive(Serialize, Deserialize, Debug)]
pub enum TxResult {
    Confirmed((Transaction, u32, BlockHeader, Height)),
    Unconfirmed(Transaction),
    Discarded,
}

/// Backend kind supported by the lampo
pub enum BackendKind {
    Core,
}

// FIXME: add the BlockSource trait for this
/// Bakend Trait specification
#[async_trait]
pub trait Backend: Send + Sync {
    /// Return the kind of backend
    fn kind(&self) -> BackendKind;

    /// Return the hash of the best block and, optionally, its height.
    ///
    /// LDK 0.3's `BlockSource` is no longer object-safe (it returns `impl
    /// Future`), so `Backend` exposes this as an object-safe async method and
    /// concrete backends implement `BlockSource` separately for chain sync.
    async fn get_best_block(&self) -> BlockSourceResult<(BlockHash, Option<u32>)>;

    /// Estimate the feerate to confirm within the given number of blocks,
    /// in sats per 1000 weight units (the unit LDK's `FeeEstimator` uses).
    ///
    /// FIXME: use `FeeRate` instead of `u32`
    async fn fee_rate_estimation(&self, blocks: u64) -> error::Result<u32>;

    /// The minimum feerate accepted into the backend's mempool, in sats
    /// per 1000 weight units.
    async fn minimum_mempool_fee(&self) -> error::Result<u32>;

    /// Broadcast the transaction to the network.
    ///
    /// Returns an error when the backend rejected or failed to relay the
    /// transaction, so callers can retry or surface the failure. Silently
    /// dropping a failed broadcast of a commitment or HTLC transaction can
    /// cost funds.
    async fn brodcast_tx(&self, tx: &Transaction) -> error::Result<()>;

    /// Broadcast a set of transactions that form a single package (a
    /// child paying for its parents), preserving order.
    ///
    /// LDK hands anchor CPFP transactions to the broadcaster together
    /// with their low-feerate commitment parent: submitting them one by
    /// one can leave the parent stuck below the mempool minimum feerate
    /// and the child rejected for missing inputs. Backends should use a
    /// package-aware RPC (`submitpackage`) when more than one transaction
    /// is given.
    async fn brodcast_txs(&self, txs: &[Transaction]) -> error::Result<()> {
        for tx in txs {
            self.brodcast_tx(tx).await?;
        }
        Ok(())
    }

    async fn get_utxo(&self, block: &BlockHash, idx: u64) -> UtxoResult;

    async fn get_utxo_by_txid(&self, txid: &Txid, script: &Script) -> error::Result<TxResult>;

    fn set_handler(&self, _: Arc<dyn Handler>) {}

    fn set_channel_manager(&self, _: Arc<LampoChannel>) {}

    fn set_chain_monitor(&self, _: Arc<LampoChainMonitor>) {}

    /// Hand over the channel monitors read from disk on restart, each
    /// paired with the best block it was persisted at. The backend must
    /// sync every monitor up to the chain tip individually and register
    /// it with the chain monitor before connecting new blocks.
    fn set_stale_monitors(&self, _: Vec<(BlockLocator, LampoMonitorListener)>) {}

    /// Register the output sweeper as a chain listener, together with the
    /// best block its persisted state was last synced to.
    fn set_sweeper(&self, _: BlockLocator, _: Arc<LampoSweeper>) {}

    /// Perform the initial chain synchronization: bring the channel
    /// manager, the chain monitor, and any stale channel monitors up to
    /// the current chain tip. Must complete before the node starts
    /// processing peers or events.
    async fn sync_chain(&self) -> error::Result<()> {
        Ok(())
    }

    /// Get the information of a transaction inside the blockchain.
    async fn get_transaction(&self, txid: &Txid) -> error::Result<TxResult>;

    /// Spawn a thread and start polling the backend and notify
    /// the listener through the handler.
    async fn listen(self: Arc<Self>) -> error::Result<()>;
}
