//! Beckend implementation
pub use bitcoin::{BlockHash, Script, Transaction, Txid};
pub use lightning::chain::WatchedOutput;
pub use lightning_block_sync::{AsyncBlockSourceResult, BlockData, BlockHeaderData};

/// Bakend Trait specification
pub trait Backend {
    /// Fetch feerate give a number of blocks
    fn fee_rate_estimation(&self, blocks: u64) -> u32;

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

    fn get_block<'a>(&'a self, header_hash: &'a BlockHash)
        -> AsyncBlockSourceResult<'a, BlockData>;

    fn get_best_block<'a>(&'a self) -> AsyncBlockSourceResult<(BlockHash, Option<u32>)>;
}
