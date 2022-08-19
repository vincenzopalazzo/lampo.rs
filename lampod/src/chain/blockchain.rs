use crate::backend::Backend;
use bitcoin::Transaction;
use lightning::chain::chaininterface::{BroadcasterInterface, ConfirmationTarget, FeeEstimator};

/// Lampo FeeEstimator implementation
struct LampoChainManager<'a> {
    backend: &'a dyn Backend,
}

impl<'a> LampoChainManager<'a> {
    /// Create a new instance of LampoFeeEstimator with the specified
    /// Backend.
    fn new(client: &'a dyn Backend) -> Self {
        LampoChainManager { backend: client }
    }
}

/// Rust lightning FeeEstimator implementation
impl FeeEstimator for LampoChainManager<'_> {
    fn get_est_sat_per_1000_weight(&self, confirmation_target: ConfirmationTarget) -> u32 {
        return match confirmation_target {
            ConfirmationTarget::Background => self.backend.fee_rate_estimation(24),
            ConfirmationTarget::Normal => self.backend.fee_rate_estimation(6),
            ConfirmationTarget::HighPriority => self.backend.fee_rate_estimation(2),
        };
    }
}

impl BroadcasterInterface for LampoChainManager<'_> {
    fn broadcast_transaction(&self, tx: &Transaction) {
        self.backend.brodcast_tx(tx);
    }
}
