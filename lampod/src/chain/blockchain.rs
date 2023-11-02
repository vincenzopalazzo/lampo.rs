use std::collections::HashMap;
use std::sync::Arc;

use bitcoin::blockdata::constants::ChainHash;
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

    fn print_ldk_target_to_string(&self, target: ConfirmationTarget) -> String {
        match target {
            ConfirmationTarget::OnChainSweep => String::from("on_chain_sweep"),
            ConfirmationTarget::AnchorChannelFee => String::from("anchor_chanenl"),
            ConfirmationTarget::NonAnchorChannelFee => String::from("non_anchor_channel"),
            ConfirmationTarget::ChannelCloseMinimum => String::from("channel_close_minimum"),
            ConfirmationTarget::MaxAllowedNonAnchorChannelRemoteFee => {
                String::from("max_allowed_anchor_channel_remote")
            }
            ConfirmationTarget::MinAllowedAnchorChannelRemoteFee => {
                String::from("min_allowed_anchor_channel_remote")
            }
            ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee => {
                String::from("min_allowed_non_anchor_channel_remote")
            }
        }
    }

    pub fn estimated_fees(&self) -> HashMap<String, Option<u32>> {
        let fees_targets = vec![
            ConfirmationTarget::OnChainSweep,
            ConfirmationTarget::MaxAllowedNonAnchorChannelRemoteFee,
            ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee,
            ConfirmationTarget::NonAnchorChannelFee,
            ConfirmationTarget::MinAllowedAnchorChannelRemoteFee,
            ConfirmationTarget::AnchorChannelFee,
            ConfirmationTarget::ChannelCloseMinimum,
        ];
        let mut map: HashMap<String, Option<u32>> = HashMap::new();
        for target in fees_targets {
            let fee = self.get_est_sat_per_1000_weight(target);
            let value = if fee == 0 { None } else { Some(fee) };
            map.insert(self.print_ldk_target_to_string(target), value);
        }
        map
    }
}

/// Rust lightning FeeEstimator implementation
impl FeeEstimator for LampoChainManager {
    fn get_est_sat_per_1000_weight(&self, confirmation_target: ConfirmationTarget) -> u32 {
        match confirmation_target {
            ConfirmationTarget::OnChainSweep => self.backend.fee_rate_estimation(1).unwrap(),
            ConfirmationTarget::MaxAllowedNonAnchorChannelRemoteFee
            | ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee
            | ConfirmationTarget::AnchorChannelFee
            | ConfirmationTarget::NonAnchorChannelFee => {
                self.backend.fee_rate_estimation(6).unwrap()
            }
            ConfirmationTarget::MinAllowedAnchorChannelRemoteFee => {
                self.backend.minimum_mempool_fee().unwrap()
            }
            ConfirmationTarget::ChannelCloseMinimum => {
                self.backend.fee_rate_estimation(100).unwrap()
            }
        }
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
        self.backend.watch_utxo(txid, script_pubkey);
    }
}

impl UtxoLookup for LampoChainManager {
    fn get_utxo(&self, _: &ChainHash, _: u64) -> lampo_common::backend::UtxoResult {
        todo!()
    }
}

// SAFETY: there is no reason why this should not be send and sync
unsafe impl Send for LampoChainManager {}
unsafe impl Sync for LampoChainManager {}
