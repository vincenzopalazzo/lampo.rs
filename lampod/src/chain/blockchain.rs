use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use lampo_common::backend::Backend;
use lampo_common::bitcoin::absolute::Height;
use lampo_common::bitcoin::blockdata::constants::ChainHash;
use lampo_common::bitcoin::Transaction;
use lampo_common::event::onchain::OnChainEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::ldk;
use lampo_common::ldk::chain::chaininterface::{
    BroadcasterInterface, ConfirmationTarget, FeeEstimator,
};
use lampo_common::ldk::chain::Filter;
use lampo_common::ldk::routing::utxo::UtxoLookup;
use lampo_common::wallet::WalletManager;
use lampo_common::{bitcoin, error};

use crate::actions::handler::LampoHandler;

#[derive(Clone)]
pub struct LampoChainManager {
    pub backend: Arc<dyn Backend>,
    pub wallet_manager: Arc<dyn WalletManager>,
    pub current_height: RefCell<Height>,
    pub best_height: RefCell<Height>,
    pub handler: RefCell<Option<Arc<LampoHandler>>>,
}

/// Personal Lampo implementation
impl LampoChainManager {
    /// Create a new instance of LampoFeeEstimator with the specified
    /// Backend.
    pub fn new(client: Arc<dyn Backend>, wallet_manager: Arc<dyn WalletManager>) -> Self {
        LampoChainManager {
            backend: client,
            wallet_manager,
            handler: RefCell::new(None),
            // Safe: 0 is a valid consensus height
            current_height: RefCell::new(Height::from_consensus(0).unwrap()),
            best_height: RefCell::new(Height::from_consensus(0).unwrap()),
        }
    }

    pub fn is_lightway(&self) -> bool {
        self.backend.is_lightway()
    }

    pub fn set_handler(&self, handler: Arc<LampoHandler>) {
        self.handler.replace(Some(handler));
    }

    pub fn handler(&self) -> Arc<LampoHandler> {
        self.handler.borrow().clone().unwrap()
    }

    pub fn is_syncing(&self) -> bool {
        let current_height = self.current_height.borrow();
        let best_height = self.best_height.borrow();
        current_height.to_consensus_u32() != best_height.to_consensus_u32()
    }

    pub fn listen(self: Arc<Self>) -> error::Result<()> {
        let _ = self.backend.clone().listen()?;
        let event_listener = self.clone();
        std::thread::spawn(move || {
            log::info!(target: "lampo_chain_manager", "listening for chain events");
            loop {
                let handler = event_listener.handler();
                let event = handler.events().recv().unwrap();
                match event {
                    Event::OnChain(OnChainEvent::NewBestBlock((_, height))) => {
                        log::debug!(target: "lampo_chain_manager", "new best block height `{}`", height);
                        *self.best_height.borrow_mut() = height;
                    }
                    Event::OnChain(OnChainEvent::NewBlock(block)) => {
                        let height = block.bip34_block_height().unwrap();
                        log::debug!(target: "lampo_chain_manager", "new block with hash `{}` at height `{}`", block.block_hash(), height);
                        let mut current_height = self.current_height.borrow_mut();
                        if height as u32 > current_height.to_consensus_u32() {
                            *current_height = Height::from_consensus(height as u32).unwrap();
                        }
                    }
                    _ => continue,
                }
            }
        });
        Ok(())
    }

    fn print_ldk_target_to_string(&self, target: ConfirmationTarget) -> String {
        match target {
            ConfirmationTarget::OnChainSweep => String::from("on_chain_sweep"),
            ConfirmationTarget::AnchorChannelFee => String::from("anchor_chanenl"),
            ConfirmationTarget::NonAnchorChannelFee => String::from("non_anchor_channel"),
            ConfirmationTarget::ChannelCloseMinimum => String::from("channel_close_minimum"),
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
    fn register_output(&self, output: ldk::chain::WatchedOutput) {
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
