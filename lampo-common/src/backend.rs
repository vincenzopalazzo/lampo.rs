//! ...
//! Beckend implementation

use std::sync::Arc;
use std::thread::JoinHandle;

use bitcoin::absolute::Height;
use bitcoin::block::Header as BlockHeader;
use serde::{Deserialize, Serialize};

pub use bitcoin::consensus::{deserialize, serialize};
pub use bitcoin::{Block, BlockHash, Script, Transaction, Txid};

use crate::error;
use crate::handler::Handler;
pub use crate::ldk::chain::WatchedOutput;
pub use crate::ldk::routing::utxo::UtxoResult;
pub use crate::ldk::sync::{
    AsyncBlockSourceResult, BlockData, BlockHeaderData, BlockSourceResult,
};

#[derive(Serialize, Deserialize, Debug)]
pub enum TxResult {
    Confirmed((Transaction, u32, BlockHeader, Height)),
    Unconfirmed(Transaction),
    Discarded,
}

/// Backend kind supported by the lampo
pub enum BackendKind {
    Core,
    Nakamoto,
}

/// Bakend Trait specification
pub trait Backend {
    /// Return the kind of backend
    fn kind(&self) -> BackendKind;

    /// Fetch feerate give a number of blocks
    fn fee_rate_estimation(&self, blocks: u64) -> error::Result<u32>;

    fn minimum_mempool_fee(&self) -> error::Result<u32>;

    fn brodcast_tx(&self, tx: &Transaction);

    fn is_lightway(&self) -> bool;

    /// You must follow this step if: you are not providing full blocks to LDK, i.e. if you're using BIP 157/158 or Electrum as your chain backend
    ///
    /// What it's used for: if you are not providing full blocks, LDK uses this object to tell you what transactions and outputs to watch for on-chain.
    fn watch_utxo(&self, txid: &Txid, script: &Script);

    /// You must follow this step if: you are not providing full blocks to LDK, i.e. if you're using BIP 157/158 or Electrum as your chain backend
    ///
    /// What it's used for: if you are not providing full blocks, LDK uses this object to tell you what transactions and outputs to watch for on-chain.
    fn register_output(&self, output: WatchedOutput) -> Option<(usize, Transaction)>;

    fn get_header<'a>(
        &'a self,
        header_hash: &'a BlockHash,
        height_hint: Option<u32>,
    ) -> AsyncBlockSourceResult<'a, BlockHeaderData>;

    fn get_block<'a>(&'a self, header_hash: &'a BlockHash) -> error::Result<BlockData>;

    fn get_best_block(&self) -> error::Result<(BlockHash, Option<u32>)>;

    fn get_utxo(&self, block: &BlockHash, idx: u64) -> UtxoResult;

    fn get_utxo_by_txid(&self, txid: &Txid, script: &Script) -> error::Result<TxResult>;

    fn set_handler(&self, _: Arc<dyn Handler>) {}

    /// Ask to the backend to watch the following UTXO and notify you
    /// when somethings changes
    fn manage_transactions(&self, txs: &mut Vec<Txid>) -> error::Result<()>;
    /// Spawn a thread and start to polling the backend and notify
    /// the listener through the handler.
    fn listen(self: Arc<Self>) -> error::Result<JoinHandle<()>>;
    /// Get the information of a transaction inside the blockchain.
    fn get_transaction(&self, txid: &Txid) -> error::Result<TxResult>;
    /// Process the transactions
    fn process_transactions(&self) -> error::Result<()>;
}
