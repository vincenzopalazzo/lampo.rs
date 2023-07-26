use std::sync::Arc;

use lampo_common::bitcoin::Transaction;
use lampo_common::ldk::chain::chaininterface::{
    BroadcasterInterface, ConfirmationTarget, FeeEstimator,
};
use lampo_common::ldk::chain::Filter;
use lampo_common::ldk::routing::utxo::UtxoLookup;

use lampo_common::backend::Backend;
use lampo_common::wallet::WalletManager;

#[derive(Clone)]
pub struct LampoChainManager {
    pub backend: Arc<dyn Backend>,
    pub wallet_manager: Arc<dyn WalletManager>,
}

/// Personal Lampo implementation
impl LampoChainManager {
    /// Create a new instance of LampoFeeEstimator with the specified
    /// Backend.
    pub fn new(client: Arc<dyn Backend>, wallet_manager: Arc<dyn WalletManager>) -> Self {
        LampoChainManager {
            backend: client,
            wallet_manager,
        }
    }

    pub fn is_lightway(&self) -> bool {
        self.backend.is_lightway()
    }
}

/// Rust lightning FeeEstimator implementation
impl FeeEstimator for LampoChainManager {
    fn get_est_sat_per_1000_weight(&self, confirmation_target: ConfirmationTarget) -> u32 {
        return match confirmation_target {
            ConfirmationTarget::Background => self.backend.fee_rate_estimation(24),
            ConfirmationTarget::Normal => self.backend.fee_rate_estimation(6),
            ConfirmationTarget::HighPriority => self.backend.fee_rate_estimation(2),
            // FIXME: use the getmempoolinfo
            ConfirmationTarget::MempoolMinimum => self.backend.fee_rate_estimation(2),
        };
    }
}

/// Brodcaster Interface implementation for Lampo.
impl BroadcasterInterface for LampoChainManager {
    fn broadcast_transactions(&self, tx: &[&Transaction]) {
        // FIXME: change the brodcasting
        self.backend.brodcast_tx(tx.first().unwrap());
    }
}

impl Filter for LampoChainManager {
    fn register_output(&self, output: lightning::chain::WatchedOutput) {
        self.backend.register_output(output);
    }

    fn register_tx(&self, txid: &bitcoin::Txid, script_pubkey: &bitcoin::Script) {
        self.backend.watch_utxo(txid, script_pubkey)
    }
}

impl UtxoLookup for LampoChainManager {
    fn get_utxo(
        &self,
        _: &bitcoin::BlockHash,
        _: u64,
    ) -> lightning::routing::utxo::UtxoResult {
        //self.backend.get_utxo(hash, idx)
        todo!()
    }
}

// SAFETY: there is no reason why this should not be send and sync
unsafe impl Send for LampoChainManager {}
unsafe impl Sync for LampoChainManager {}
