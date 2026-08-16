use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use lampo_common::async_trait;
use lampo_common::backend::{Backend, FeeEstimateMode};
use lampo_common::bitcoin;
use lampo_common::bitcoin::blockdata::constants::ChainHash;
use lampo_common::bitcoin::{FeeRate, Transaction};
use lampo_common::conf::Network;
use lampo_common::error;
use lampo_common::ldk::chain::chaininterface::{
    BroadcasterInterface, ConfirmationTarget, FeeEstimator, TransactionType,
};
use lampo_common::ldk::routing::utxo::UtxoLookup;
use lampo_common::ldk::util::wakers::Notifier;
use lampo_common::wallet::WalletManager;

use super::fee::{
    all_targets, apply_post_estimation_adjustments, source_for_target, FeeCache, FeeSource,
    FeeTarget, FEE_CACHE_REFRESH_SECS, FEE_CACHE_UPDATE_TIMEOUT_SECS, RELAY_FALLBACK_SAT_PER_KW,
};

#[derive(Clone)]
pub struct LampoChainManager {
    pub backend: Arc<dyn Backend>,
    pub wallet_manager: Arc<dyn WalletManager>,
    network: Network,
    fee_cache: Arc<FeeCache>,
    fee_refresh_started: Arc<AtomicBool>,
    /// Same flag as [`crate::LampoDaemon::shutdown`]. The refresh task
    /// holds only a `Weak` so dropping the daemon also stops the loop.
    shutdown: Arc<AtomicBool>,
}

/// Personal Lampo implementation
impl LampoChainManager {
    /// Create a new instance of LampoFeeEstimator with the specified
    /// Backend.
    pub fn new(
        client: Arc<dyn Backend>,
        wallet_manager: Arc<dyn WalletManager>,
        network: Network,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        LampoChainManager {
            backend: client,
            wallet_manager,
            network,
            fee_cache: Arc::new(FeeCache::new()),
            fee_refresh_started: Arc::new(AtomicBool::new(false)),
            shutdown,
        }
    }

    pub fn estimated_fees(&self) -> HashMap<String, Option<u32>> {
        let mut map: HashMap<String, Option<u32>> = HashMap::new();
        for target in all_targets() {
            let fee = self.fee_cache.get(target);
            let value = if fee == 0 { None } else { Some(fee) };
            map.insert(print_fee_target(target), value);
        }
        map
    }

    /// Sync feerate for wallet constructions. ldk-node's
    /// `OnchainFeeEstimator::estimate_fee_rate`.
    pub fn estimate_fee_rate(&self, target: FeeTarget) -> FeeRate {
        FeeRate::from_sat_per_kwu(self.fee_cache.get(target) as u64)
    }

    pub fn listen(self: Arc<Self>) -> error::Result<()> {
        let backend = self.backend.clone();
        tokio::spawn(async move { backend.listen().await });
        self.spawn_fee_refresh();
        Ok(())
    }

    /// Start the background refresh if it is not already running. Safe to call
    /// from `init_onchaind` so the cache warms during the rest of startup,
    /// and again from `listen`.
    pub fn spawn_fee_refresh(self: Arc<Self>) {
        if self.fee_refresh_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(&self);
        tokio::spawn(async move { run_fee_refresh(weak).await });
    }

    fn should_stop(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    async fn refresh_fee_cache(&self) -> bool {
        if self.should_stop() {
            return false;
        }
        let now = Instant::now();
        let mut by_key: HashMap<(u64, FeeEstimateMode), u32> = HashMap::new();
        let mut updates = HashMap::with_capacity(10);
        for target in all_targets() {
            if self.should_stop() {
                return false;
            }
            let sat_kw = match self.estimate_sat_kw(target, &mut by_key).await {
                Ok(sat_kw) => sat_kw,
                Err(err) => match self.on_estimate_error(target, &err) {
                    Some(sat_kw) => sat_kw,
                    None => return true,
                },
            };
            let sat_kw = apply_post_estimation_adjustments(target, sat_kw);
            log::debug!(
                target: "lampo-chain",
                "fee cache {target:?}: {sat_kw} sat/kW"
            );
            updates.insert(target, sat_kw);
        }
        if self.fee_cache.set(updates) {
            log::info!(
                target: "lampo-chain",
                "fee rate cache update finished in {}ms",
                now.elapsed().as_millis()
            );
        }
        true
    }

    async fn estimate_sat_kw(
        &self,
        target: FeeTarget,
        by_key: &mut HashMap<(u64, FeeEstimateMode), u32>,
    ) -> error::Result<u32> {
        match source_for_target(target) {
            FeeSource::MempoolMin => timeout_rpc(self.backend.minimum_mempool_fee()).await,
            FeeSource::Blocks { blocks, mode } => {
                if let Some(sat_kw) = by_key.get(&(blocks, mode)) {
                    return Ok(*sat_kw);
                }
                let sat_kw =
                    timeout_rpc(self.backend.fee_rate_estimation_with_mode(blocks, mode)).await?;
                by_key.insert((blocks, mode), sat_kw);
                Ok(sat_kw)
            }
        }
    }

    /// ldk-node: Bitcoin fails the whole update; regtest/signet fall back to
    /// 1 sat/vB; testnet skips the update and keeps the previous cache.
    fn on_estimate_error(&self, target: FeeTarget, err: &error::Error) -> Option<u32> {
        match self.network {
            Network::Bitcoin => {
                log::error!(
                    target: "lampo-chain",
                    "fee estimate for {target:?} failed on bitcoin: {err}"
                );
                None
            }
            Network::Regtest | Network::Signet => {
                log::warn!(
                    target: "lampo-chain",
                    "fee estimate for {target:?} failed: {err}. Falling back to 1 sat/vB"
                );
                Some(RELAY_FALLBACK_SAT_PER_KW)
            }
            _ => {
                log::warn!(
                    target: "lampo-chain",
                    "fee estimate for {target:?} failed: {err}. Keeping previous cache"
                );
                None
            }
        }
    }
}

fn print_fee_target(target: FeeTarget) -> String {
    match target {
        FeeTarget::OnchainPayment => String::from("onchain_payment"),
        FeeTarget::ChannelFunding => String::from("channel_funding"),
        FeeTarget::Lightning(ConfirmationTarget::MaximumFeeEstimate) => String::from("maximum"),
        FeeTarget::Lightning(ConfirmationTarget::UrgentOnChainSweep) => String::from("urgent"),
        FeeTarget::Lightning(ConfirmationTarget::AnchorChannelFee) => {
            String::from("anchor_channel")
        }
        FeeTarget::Lightning(ConfirmationTarget::NonAnchorChannelFee) => {
            String::from("non_anchor_channel")
        }
        FeeTarget::Lightning(ConfirmationTarget::ChannelCloseMinimum) => {
            String::from("channel_close_minimum")
        }
        FeeTarget::Lightning(ConfirmationTarget::MinAllowedAnchorChannelRemoteFee) => {
            String::from("min_allowed_anchor_channel_remote")
        }
        FeeTarget::Lightning(ConfirmationTarget::MinAllowedNonAnchorChannelRemoteFee) => {
            String::from("min_allowed_non_anchor_channel_remote")
        }
        FeeTarget::Lightning(ConfirmationTarget::OutputSpendingFee) => {
            String::from("output_spending")
        }
    }
}

async fn timeout_rpc<F, T>(fut: F) -> error::Result<T>
where
    F: std::future::Future<Output = error::Result<T>>,
{
    tokio::time::timeout(Duration::from_secs(FEE_CACHE_UPDATE_TIMEOUT_SECS), fut)
        .await
        .map_err(|_| error::anyhow!("fee estimate timed out"))?
}

/// Background loop for [`LampoChainManager::spawn_fee_refresh`].
///
/// Holds a `Weak` so the task does not keep the chain manager (and therefore
/// the wallet / backend) alive after the daemon is dropped. Observes the
/// daemon shutdown flag with the same 100ms poll as the LDK event processor,
/// so a stop does not wait out the 10-minute interval.
async fn run_fee_refresh(weak: Weak<LampoChainManager>) {
    loop {
        let Some(this) = weak.upgrade() else {
            return;
        };
        if this.should_stop() {
            log::info!(target: "lampo-chain", "fee cache refresh stopped");
            return;
        }
        let keep_going = this.refresh_fee_cache().await;
        drop(this);
        if !keep_going {
            log::info!(target: "lampo-chain", "fee cache refresh stopped");
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(FEE_CACHE_REFRESH_SECS)) => {}
            _ = wait_until_fee_refresh_stopped(&weak) => {
                log::info!(target: "lampo-chain", "fee cache refresh stopped");
                return;
            }
        }
    }
}

async fn wait_until_fee_refresh_stopped(weak: &Weak<LampoChainManager>) {
    loop {
        match weak.upgrade() {
            None => return,
            Some(this) if this.should_stop() => return,
            Some(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

/// Rust lightning FeeEstimator implementation
#[async_trait]
impl FeeEstimator for LampoChainManager {
    fn get_est_sat_per_1000_weight(&self, confirmation_target: ConfirmationTarget) -> u32 {
        self.fee_cache.get(confirmation_target.into())
    }
}

/// Brodcaster Interface implementation for Lampo.
impl BroadcasterInterface for LampoChainManager {
    fn broadcast_transactions(&self, txs: &[(&Transaction, TransactionType)]) {
        // FIXME: support brodcast_txs for multiple tx
        // FIXME: we are missing any error in the brodcast_tx, we should
        // fix that
        for (tx, _) in txs.to_vec() {
            let tx = tx.clone();
            let backend = self.backend.clone();
            tokio::spawn(async move {
                let tx = tx.clone();
                backend.brodcast_tx(&tx).await;
            });
        }
    }
}

impl UtxoLookup for LampoChainManager {
    fn get_utxo(
        &self,
        _: &ChainHash,
        _: u64,
        _: Arc<Notifier>,
    ) -> lampo_common::backend::UtxoResult {
        unimplemented!()
    }
}

#[async_trait]
impl Backend for LampoChainManager {
    async fn brodcast_tx(&self, tx: &Transaction) {
        self.backend.brodcast_tx(tx).await;
    }

    async fn fee_rate_estimation_with_mode(
        &self,
        blocks: u64,
        mode: lampo_common::backend::FeeEstimateMode,
    ) -> lampo_common::error::Result<u32> {
        self.backend
            .fee_rate_estimation_with_mode(blocks, mode)
            .await
    }

    async fn get_transaction(
        &self,
        txid: &bitcoin::Txid,
    ) -> lampo_common::error::Result<lampo_common::backend::TxResult> {
        self.backend.get_transaction(txid).await
    }

    async fn get_utxo(
        &self,
        block: &bitcoin::BlockHash,
        idx: u64,
    ) -> lampo_common::backend::UtxoResult {
        Backend::get_utxo(self.backend.as_ref(), block, idx).await
    }

    async fn get_utxo_by_txid(
        &self,
        txid: &bitcoin::Txid,
        script: &bitcoin::Script,
    ) -> lampo_common::error::Result<lampo_common::backend::TxResult> {
        self.backend.get_utxo_by_txid(txid, script).await
    }

    fn kind(&self) -> lampo_common::backend::BackendKind {
        self.backend.kind()
    }

    async fn get_best_block(
        &self,
    ) -> lampo_common::backend::BlockSourceResult<(bitcoin::BlockHash, Option<u32>)> {
        self.backend.get_best_block().await
    }

    async fn listen(self: Arc<Self>) -> lampo_common::error::Result<()> {
        self.backend.clone().listen().await
    }

    async fn minimum_mempool_fee(&self) -> lampo_common::error::Result<u32> {
        self.backend.minimum_mempool_fee().await
    }

    fn set_handler(&self, arc: Arc<dyn lampo_common::handler::Handler>) {
        self.backend.set_handler(arc);
    }

    // Forward every injection hook to the wrapped backend. The `Backend`
    // trait defaults them to no-ops, so a facade that does not forward
    // silently swallows the injection and the coordinator/wallet never
    // reaches the real backend.
    fn set_channel_manager(&self, channel_manager: Arc<lampo_common::types::LampoChannel>) {
        self.backend.set_channel_manager(channel_manager);
    }

    fn set_chain_monitor(&self, chain_monitor: Arc<lampo_common::types::LampoChainMonitor>) {
        self.backend.set_chain_monitor(chain_monitor);
    }

    fn set_coordinator(&self, coordinator: Arc<lampo_common::chainsync::ChainSyncCoordinator>) {
        self.backend.set_coordinator(coordinator);
    }

    fn set_wallet_manager(&self, wallet: Arc<dyn WalletManager>) {
        self.backend.set_wallet_manager(wallet);
    }
}
