//! Beckend implementation
use bitcoin::Transaction;

/// Bakend Trait specification
pub trait Backend {
    /// Fetch feerate give a number of blocks
    fn fee_rate_estimation(&self, blocks: u64) -> u32;

    fn brodcast_tx(&self, tx: &Transaction);

    fn is_lightway(&self) -> bool;
}
