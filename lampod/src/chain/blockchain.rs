use core::sync;
use std::collections::HashMap;
use std::sync::Arc;

use lampo_common::backend::Backend;
use lampo_common::bitcoin;
use lampo_common::bitcoin::blockdata::constants::ChainHash;
use lampo_common::bitcoin::Transaction;
use lampo_common::ldk;
use lampo_common::ldk::block_sync::BlockSource;
use lampo_common::ldk::chain::chaininterface::{
    BroadcasterInterface, ConfirmationTarget, FeeEstimator,
};
use lampo_common::ldk::routing::utxo::UtxoLookup;
use lampo_common::wallet::WalletManager;

use crate::sync;

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

    fn print_ldk_target_to_string(&self, target: ConfirmationTarget) -> String {
        match target {
            ConfirmationTarget::OnChainSweep => String::from("on_chain_sweep"),
            ConfirmationTarget::AnchorChannelFee => String::from("anchor_channel"),
            ConfirmationTarget::NonAnchorChannelFee => String::from("non_anchor_channel"),
            ConfirmationTarget::ChannelCloseMinimum => String::from("channel_close_minimum"),
            ConfirmationTarget::MinAllowedAnchorChannelRemoteFee => {
                String::from("min_allowed_anchor_channel_remote")
            }
            ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee => {
                String::from("min_allowed_non_anchor_channel_remote")
            }
            ConfirmationTarget::OutputSpendingFee => String::from("output_spending"),
        }
    }

    pub fn estimated_fees(&self) -> HashMap<String, Option<u32>> {
        let fees_targets = vec![
            ConfirmationTarget::OnChainSweep,
            ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee,
            ConfirmationTarget::NonAnchorChannelFee,
            ConfirmationTarget::MinAllowedAnchorChannelRemoteFee,
            ConfirmationTarget::AnchorChannelFee,
            ConfirmationTarget::ChannelCloseMinimum,
            ConfirmationTarget::OutputSpendingFee,
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
        //FIXME: use cache to avoid return default value (that is 0) on u32
        match confirmation_target {
            ConfirmationTarget::OnChainSweep => {
                self.backend.fee_rate_estimation(1).unwrap_or_default()
            }
            ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee
            | ConfirmationTarget::AnchorChannelFee
            | ConfirmationTarget::NonAnchorChannelFee => {
                self.backend.fee_rate_estimation(6).unwrap_or_default()
            }
            ConfirmationTarget::MinAllowedAnchorChannelRemoteFee => {
                self.backend.minimum_mempool_fee().unwrap()
            }
            ConfirmationTarget::ChannelCloseMinimum => {
                self.backend.fee_rate_estimation(100).unwrap_or_default()
            }
            ConfirmationTarget::OutputSpendingFee => {
                self.backend.fee_rate_estimation(12).unwrap_or_default()
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

impl BlockSource for LampoChainManager {
    fn get_best_block<'a>(
        &'a self,
    ) -> ldk::block_sync::AsyncBlockSourceResult<(bitcoin::BlockHash, Option<u32>)> {
        sync!(self.backend.get_best_block().await)
    }

    fn get_block<'a>(
        &'a self,
        header_hash: &'a bitcoin::BlockHash,
    ) -> ldk::block_sync::AsyncBlockSourceResult<'a, ldk::block_sync::BlockData> {
        sync!(self.backend.get_block(header_hash).await)
    }

    fn get_header<'a>(
        &'a self,
        header_hash: &'a bitcoin::BlockHash,
        height_hint: Option<u32>,
    ) -> ldk::block_sync::AsyncBlockSourceResult<'a, ldk::block_sync::BlockHeaderData> {
        sync!(self.backend.get_header(header_hash, height_hint).await)
    }
}

impl UtxoLookup for LampoChainManager {
    fn get_utxo(&self, _: &ChainHash, _: u64) -> lampo_common::backend::UtxoResult {
        unimplemented!()
    }
}

// SAFETY: there is no reason why this should not be send and sync
unsafe impl Send for LampoChainManager {}
unsafe impl Sync for LampoChainManager {}

impl Backend for LampoChainManager {
    fn brodcast_tx(&self, tx: &Transaction) {
        self.backend.brodcast_tx(tx);
    }

    fn fee_rate_estimation(&self, blocks: u64) -> lampo_common::error::Result<u32> {
        self.backend.fee_rate_estimation(blocks)
    }

    fn get_transaction(
        &self,
        txid: &bitcoin::Txid,
    ) -> lampo_common::error::Result<lampo_common::backend::TxResult> {
        self.backend.get_transaction(txid)
    }

    fn get_utxo(&self, block: &bitcoin::BlockHash, idx: u64) -> lampo_common::backend::UtxoResult {
        Backend::get_utxo(self.backend.as_ref(), block, idx)
    }

    fn get_utxo_by_txid(
        &self,
        txid: &bitcoin::Txid,
        script: &bitcoin::Script,
    ) -> lampo_common::error::Result<lampo_common::backend::TxResult> {
        self.backend.get_utxo_by_txid(txid, script)
    }

    fn kind(&self) -> lampo_common::backend::BackendKind {
        self.backend.kind()
    }

    fn listen(self: Arc<Self>) -> lampo_common::error::Result<()> {
        self.backend.clone().listen()
    }

    fn minimum_mempool_fee(&self) -> lampo_common::error::Result<u32> {
        self.backend.minimum_mempool_fee()
    }

    fn set_handler(&self, arc: Arc<dyn lampo_common::handler::Handler>) {
        self.backend.set_handler(arc);
    }
}
