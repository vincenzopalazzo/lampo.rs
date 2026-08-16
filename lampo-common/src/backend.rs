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

use crate::chainsync::ChainSyncCoordinator;
use crate::error;
use crate::handler::Handler;
use crate::types::{LampoChainMonitor, LampoChannel};
use crate::wallet::WalletManager;

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

/// bitcoind `estimatesmartfee` estimate mode. Matches ldk-node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeeEstimateMode {
    Conservative,
    Economical,
}

impl FeeEstimateMode {
    pub fn as_core_str(self) -> &'static str {
        match self {
            Self::Conservative => "CONSERVATIVE",
            Self::Economical => "ECONOMICAL",
        }
    }
}

/// bitcoind `estimatesmartfee` / `mempoolminfee` is BTC/kvB.
///
/// sat/kW = BTC/kvB * 1e8 / 4 = BTC/kvB * 25_000_000. Round at sat/kW,
/// same as ldk-node, so a 1.001 sat/vB mempool floor stays ~250 sat/kW
/// (then LDK's 253 floor) instead of ceiling to 2 sat/vB = 500 sat/kW.
pub fn btc_per_kvb_to_sat_per_kw(btc_per_kvb: f64) -> error::Result<u32> {
    if !btc_per_kvb.is_finite() || btc_per_kvb < 0.0 {
        return Err(error::anyhow!("invalid feerate {btc_per_kvb} BTC/kvB"));
    }
    let sat_kw = (btc_per_kvb * 25_000_000.0).round();
    if sat_kw > u32::MAX as f64 {
        return Err(error::anyhow!("feerate overflow: {btc_per_kvb} BTC/kvB"));
    }
    Ok(sat_kw as u32)
}

#[cfg(test)]
mod tests {
    use super::btc_per_kvb_to_sat_per_kw;

    #[test]
    fn one_sat_per_vb_is_250_sat_per_kw() {
        assert_eq!(btc_per_kvb_to_sat_per_kw(0.00001).unwrap(), 250);
    }

    #[test]
    fn fractional_sat_per_vb_does_not_ceil_to_two() {
        // 0.00001001 BTC/kvB = 1.001 sat/vB. Ceiling that to 2 sat/vB
        // then * 250 would install 500 sat/kW and reject a 253 funder.
        assert_eq!(btc_per_kvb_to_sat_per_kw(0.00001001).unwrap(), 250);
    }
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

    /// Fetch feerate give a number of blocks
    ///
    /// Returns the feerate in **sat/kW** (LDK's `sat_per_1000_weight`).
    /// Convert bitcoind's BTC/kvB with [`btc_per_kvb_to_sat_per_kw`] so we
    /// do not round through integer sat/vB first.
    ///
    /// FIXME: use `FeeRate` instead of `u32`
    async fn fee_rate_estimation(&self, blocks: u64) -> error::Result<u32> {
        self.fee_rate_estimation_with_mode(blocks, FeeEstimateMode::Conservative)
            .await
    }

    /// Like [`Self::fee_rate_estimation`], with bitcoind's `estimate_mode`.
    ///
    /// ldk-node uses `CONSERVATIVE` for MaximumFeeEstimate and UrgentOnChainSweep,
    /// `ECONOMICAL` otherwise.
    async fn fee_rate_estimation_with_mode(
        &self,
        blocks: u64,
        mode: FeeEstimateMode,
    ) -> error::Result<u32>;

    /// Current mempool minimum feerate in **sat/kW**.
    async fn minimum_mempool_fee(&self) -> error::Result<u32>;

    async fn brodcast_tx(&self, tx: &Transaction);

    async fn get_utxo(&self, block: &BlockHash, idx: u64) -> UtxoResult;

    async fn get_utxo_by_txid(&self, txid: &Txid, script: &Script) -> error::Result<TxResult>;

    fn set_handler(&self, _: Arc<dyn Handler>) {}

    fn set_channel_manager(&self, _: Arc<LampoChannel>) {}

    fn set_chain_monitor(&self, _: Arc<LampoChainMonitor>) {}

    /// Inject the backend-agnostic chain-sync coordinator so the backend can
    /// publish listener-sync progress. Default no-op.
    fn set_coordinator(&self, _: Arc<ChainSyncCoordinator>) {}

    /// Inject the on-chain wallet so the backend can drive it through the same
    /// chain sync as the LDK listeners (one RPC stream). Default no-op. Passed
    /// as the lampo-native `WalletManager`; the backend never sees BDK types.
    fn set_wallet_manager(&self, _: Arc<dyn WalletManager>) {}

    /// Get the information of a transaction inside the blockchain.
    async fn get_transaction(&self, txid: &Txid) -> error::Result<TxResult>;

    /// Spawn a thread and start polling the backend and notify
    /// the listener through the handler.
    async fn listen(self: Arc<Self>) -> error::Result<()>;
}
