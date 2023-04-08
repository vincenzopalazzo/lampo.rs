use std::sync::Arc;

use bitcoin::Transaction;

use lightning::chain::chaininterface::{BroadcasterInterface, ConfirmationTarget, FeeEstimator};
use lightning::chain::keysinterface::KeysManager;
use lightning::chain::Filter;

use crate::backend::Backend;
use crate::keys::keys::LampoKeys;
use crate::persistence::LampoPersistence;

/// Lampo FeeEstimator implementation
#[derive(Clone)]
pub struct LampoChainManager {
    pub backend: Arc<dyn Backend>,
    persister: Option<Arc<LampoPersistence>>,
    pub keymanager: Arc<LampoKeys>,
}

/// Personal Lampo implementation
impl LampoChainManager {
    /// Create a new instance of LampoFeeEstimator with the specified
    /// Backend.
    fn new<'c>(client: Arc<dyn Backend>, keys: Arc<LampoKeys>) -> Self {
        LampoChainManager {
            backend: client,
            persister: None,
            keymanager: keys,
        }
    }
}

/// Rust lightning FeeEstimator implementation
impl FeeEstimator for LampoChainManager {
    fn get_est_sat_per_1000_weight(&self, confirmation_target: ConfirmationTarget) -> u32 {
        return match confirmation_target {
            ConfirmationTarget::Background => self.backend.fee_rate_estimation(24),
            ConfirmationTarget::Normal => self.backend.fee_rate_estimation(6),
            ConfirmationTarget::HighPriority => self.backend.fee_rate_estimation(2),
        };
    }
}

/// Brodcaster Interface implementation for Lampo.
impl BroadcasterInterface for LampoChainManager {
    fn broadcast_transaction(&self, tx: &Transaction) {
        self.backend.brodcast_tx(tx);
    }
}

// FIXME: todo implement it.
impl Filter for LampoChainManager {
    fn register_output(&self, output: lightning::chain::WatchedOutput) {}

    fn register_tx(&self, txid: &bitcoin::Txid, script_pubkey: &bitcoin::Script) {}
}

impl lightning_block_sync::BlockSource for LampoChainManager {
    fn get_best_block<'a>(
        &'a self,
    ) -> lightning_block_sync::AsyncBlockSourceResult<(bitcoin::BlockHash, Option<u32>)> {
        self.backend.get_best_block()
    }

    fn get_block<'a>(
        &'a self,
        header_hash: &'a bitcoin::BlockHash,
    ) -> lightning_block_sync::AsyncBlockSourceResult<'a, lightning_block_sync::BlockData> {
        self.backend.get_block(header_hash)
    }

    fn get_header<'a>(
        &'a self,
        header_hash: &'a bitcoin::BlockHash,
        height_hint: Option<u32>,
    ) -> lightning_block_sync::AsyncBlockSourceResult<'a, lightning_block_sync::BlockHeaderData>
    {
        self.get_header(header_hash, height_hint)
    }
}

// FIXME: fix this
unsafe impl Send for LampoChainManager {}
unsafe impl Sync for LampoChainManager {}
