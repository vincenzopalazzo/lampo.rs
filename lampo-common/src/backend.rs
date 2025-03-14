//! ...
//! Beckend implementation
use std::sync::Arc;

use bitcoin::absolute::Height;
use bitcoin::block::Header as BlockHeader;

pub use bitcoin::consensus::{deserialize, serialize};
pub use bitcoin::{Block, BlockHash, Script, Transaction, Txid};
pub use lightning::chain::WatchedOutput;
pub use lightning::routing::utxo::UtxoResult;
use lightning_block_sync::BlockSource;
pub use lightning_block_sync::{
    AsyncBlockSourceResult, BlockData, BlockHeaderData, BlockSourceResult,
};
use serde::{Deserialize, Serialize};

use crate::error;
use crate::handler::Handler;
use crate::types::{LampoChainMonitor, LampoChannel};

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
pub trait Backend: BlockSource + Send + Sync {
    /// Return the kind of backend
    fn kind(&self) -> BackendKind;

    /// Fetch feerate give a number of blocks
    fn fee_rate_estimation(&self, blocks: u64) -> error::Result<u32>;

    fn minimum_mempool_fee(&self) -> error::Result<u32>;

    fn brodcast_tx(&self, tx: &Transaction);

    fn get_utxo(&self, block: &BlockHash, idx: u64) -> UtxoResult;

    fn get_utxo_by_txid(&self, txid: &Txid, script: &Script) -> error::Result<TxResult>;

    fn set_handler(&self, _: Arc<dyn Handler>) {}

    fn set_channel_manager(&self, _: Arc<LampoChannel>) {}

    fn set_chain_monitor(&self, _: Arc<LampoChainMonitor>) {}

    /// Get the information of a transaction inside the blockchain.
    fn get_transaction(&self, txid: &Txid) -> error::Result<TxResult>;

    /// Spawn a thread and start polling the backend and notify
    /// the listener through the handler.
    fn listen(self: Arc<Self>) -> error::Result<()>;
}
