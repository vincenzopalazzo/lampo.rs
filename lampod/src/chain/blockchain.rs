use std::sync::Arc;

use bitcoin::Transaction;

use lightning::chain::chaininterface::{BroadcasterInterface, ConfirmationTarget, FeeEstimator};
use lightning::chain::chainmonitor::ChainMonitor;
use lightning::chain::channelmonitor::ChannelMonitor;
use lightning::chain::keysinterface::InMemorySigner;
use lightning::chain::Filter;
use lightning_persister::FilesystemPersister;

use crate::backend::Backend;
use crate::keys::keys::LampoKeys;
use crate::persistence::LampoPersistence;
use crate::utils::logger::LampoLogger;

type LampoChannelMonitor = ChainMonitor<
    InMemorySigner,
    Arc<dyn Filter + Send + Sync>,
    Arc<LampoChainManager>,
    Arc<LampoChainManager>,
    Arc<LampoLogger>,
    Arc<FilesystemPersister>,
>;

#[derive(Clone)]
/// Lampo FeeEstimator implementation
struct LampoChainManager {
    backend: Arc<dyn Backend>,
    persister: Option<Arc<LampoPersistence>>,
    keymanager: Arc<LampoKeys>,
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

    fn build(
        &mut self,
        logger: &LampoLogger,
        filter: Arc<dyn Filter + Send + Sync>,
        persister: LampoPersistence,
    ) -> Arc<LampoChannelMonitor> {
        let per = persister.clone().persister;
        self.persister = Some(Arc::new(persister));
        Arc::new(ChainMonitor::new(
            Some(filter),
            Arc::new(self.clone()),
            Arc::new(logger.clone()),
            Arc::new(self.clone()),
            Arc::new(per),
        ))
    }

    fn reload(&self) -> Vec<(bitcoin::BlockHash, ChannelMonitor<InMemorySigner>)> {
        let channel_monitors = self
            .persister
            .clone()
            .unwrap()
            .persister
            .read_channelmonitors(&self.keymanager.keys_manager, &self.keymanager.keys_manager)
            .unwrap();
        if self.backend.is_lightway() {
            for (_, channel_monitor) in channel_monitors.iter() {
                channel_monitor.load_outputs_to_watch(&self);
            }
        }
        channel_monitors
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
