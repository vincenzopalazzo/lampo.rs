use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use lampo_common::async_trait;
use lampo_common::backend::Backend;
use lampo_common::bitcoin;
use lampo_common::bitcoin::blockdata::constants::ChainHash;
use lampo_common::bitcoin::Transaction;
use lampo_common::error;
use lampo_common::ldk::chain::chaininterface::{
    BroadcasterInterface, ConfirmationTarget, FeeEstimator, TransactionType,
    FEERATE_FLOOR_SATS_PER_KW,
};
use lampo_common::ldk::routing::utxo::UtxoLookup;
use lampo_common::ldk::util::wakers::Notifier;
use lampo_common::wallet::WalletManager;

/// How often the fee cache is refreshed from the backend.
const FEE_REFRESH_INTERVAL_SECS: u64 = 60;

/// Conservative defaults, in sats per 1000 weight units, used until the
/// first successful refresh from the backend.
const DEFAULT_URGENT_SAT_PER_KW: u32 = 5_000;
const DEFAULT_NORMAL_SAT_PER_KW: u32 = 3_000;
const DEFAULT_BACKGROUND_SAT_PER_KW: u32 = 1_000;

/// Fee rates cached from the backend, in sats per 1000 weight units.
///
/// A value of `0` means "not fetched yet" and falls back to a conservative
/// per-target default: [`FeeEstimator::get_est_sat_per_1000_weight`] is a
/// synchronous callback invoked by LDK from commitment/HTLC signing paths,
/// so it must never block on the backend.
#[derive(Default)]
struct FeeCache {
    /// Confirmation within ~3 blocks, used for urgent on-chain sweeps.
    urgent: AtomicU32,
    /// Confirmation within ~6 blocks, used for commitment transactions.
    normal: AtomicU32,
    /// Confirmation within ~144 blocks, used where confirmation can wait
    /// (channel close minimum, anchor commitments bumped via CPFP).
    background: AtomicU32,
    /// The backend's minimum mempool fee, used as the lower bound we
    /// require from the counterparty's feerate.
    mempool_min: AtomicU32,
}

#[derive(Clone)]
pub struct LampoChainManager {
    pub backend: Arc<dyn Backend>,
    pub wallet_manager: Arc<dyn WalletManager>,
    fees: Arc<FeeCache>,
}

/// Personal Lampo implementation
impl LampoChainManager {
    /// Create a new instance of LampoFeeEstimator with the specified
    /// Backend.
    pub fn new(client: Arc<dyn Backend>, wallet_manager: Arc<dyn WalletManager>) -> Self {
        LampoChainManager {
            backend: client,
            wallet_manager,
            fees: Arc::new(FeeCache::default()),
        }
    }

    /// Refresh the fee cache from the backend. Each bucket is refreshed
    /// independently so a single failing estimate does not blank the others.
    async fn refresh_fee_cache(&self) {
        let buckets = [
            (&self.fees.urgent, 3u64, "urgent"),
            (&self.fees.normal, 6u64, "normal"),
            (&self.fees.background, 144u64, "background"),
        ];
        for (cell, blocks, label) in buckets {
            match self.backend.fee_rate_estimation(blocks).await {
                Ok(fee) => cell.store(fee.max(FEERATE_FLOOR_SATS_PER_KW), Ordering::Release),
                Err(err) => {
                    log::warn!(target: "lampo-chain", "fee estimation for `{label}` ({blocks} blocks) failed: {err}")
                }
            }
        }
        match self.backend.minimum_mempool_fee().await {
            Ok(fee) => self
                .fees
                .mempool_min
                .store(fee.max(FEERATE_FLOOR_SATS_PER_KW), Ordering::Release),
            Err(err) => {
                log::warn!(target: "lampo-chain", "minimum mempool fee estimation failed: {err}")
            }
        }
    }

    fn print_ldk_target_to_string(&self, target: ConfirmationTarget) -> String {
        match target {
            ConfirmationTarget::MaximumFeeEstimate => String::from("maximum"),
            ConfirmationTarget::UrgentOnChainSweep => String::from("urgent"),
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
            ConfirmationTarget::UrgentOnChainSweep,
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

    pub fn listen(self: Arc<Self>) -> error::Result<()> {
        let fee_refresher = self.clone();
        tokio::spawn(async move {
            loop {
                fee_refresher.refresh_fee_cache().await;
                tokio::time::sleep(std::time::Duration::from_secs(FEE_REFRESH_INTERVAL_SECS)).await;
            }
        });
        tokio::spawn(async move { self.backend.clone().listen().await });
        Ok(())
    }
}

/// Rust lightning FeeEstimator implementation
#[async_trait]
impl FeeEstimator for LampoChainManager {
    fn get_est_sat_per_1000_weight(&self, confirmation_target: ConfirmationTarget) -> u32 {
        use ConfirmationTarget::*;

        let (cell, default) = match confirmation_target {
            MaximumFeeEstimate | UrgentOnChainSweep => {
                (&self.fees.urgent, DEFAULT_URGENT_SAT_PER_KW)
            }
            OutputSpendingFee | NonAnchorChannelFee => {
                (&self.fees.normal, DEFAULT_NORMAL_SAT_PER_KW)
            }
            AnchorChannelFee | ChannelCloseMinimum => {
                (&self.fees.background, DEFAULT_BACKGROUND_SAT_PER_KW)
            }
            MinAllowedAnchorChannelRemoteFee | MinAllowedNonAnchorChannelRemoteFee => {
                (&self.fees.mempool_min, FEERATE_FLOOR_SATS_PER_KW)
            }
        };
        let value = cell.load(Ordering::Acquire);
        let value = if value == 0 { default } else { value };
        // `MaximumFeeEstimate` bounds the feerate we tolerate from the
        // counterparty; LDK recommends adding headroom over the raw
        // estimate so honest peers are not force-closed during fee spikes.
        let value = if matches!(confirmation_target, MaximumFeeEstimate) {
            value.saturating_mul(2)
        } else {
            value
        };
        value.max(FEERATE_FLOOR_SATS_PER_KW)
    }
}

/// How many times a failed broadcast is retried before giving up. LDK
/// re-broadcasts pending claims on its own timer, so this only needs to
/// cover transient backend failures (e.g. a bitcoind restart).
const BROADCAST_ATTEMPTS: u32 = 3;
const BROADCAST_RETRY_DELAY_SECS: u64 = 5;

/// Brodcaster Interface implementation for Lampo.
impl BroadcasterInterface for LampoChainManager {
    fn broadcast_transactions(&self, txs: &[(&Transaction, TransactionType)]) {
        // When LDK hands over more than one transaction they form a
        // package (a child paying for its parents, e.g. an anchor CPFP
        // and its commitment) and must be relayed together, in order.
        let txs: Vec<Transaction> = txs.iter().map(|(tx, _)| (*tx).clone()).collect();
        let backend = self.backend.clone();
        tokio::spawn(async move {
            let txids = txs
                .iter()
                .map(|tx| tx.compute_txid().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            for attempt in 1..=BROADCAST_ATTEMPTS {
                match backend.brodcast_txs(&txs).await {
                    Ok(()) => return,
                    Err(err) if attempt < BROADCAST_ATTEMPTS => {
                        log::warn!(
                            target: "lampo-chain",
                            "broadcast of `{txids}` failed (attempt {attempt}/{BROADCAST_ATTEMPTS}): {err}"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(
                            BROADCAST_RETRY_DELAY_SECS,
                        ))
                        .await;
                    }
                    Err(err) => {
                        // This can be a commitment or HTLC transaction:
                        // scream, do not whisper. LDK will retry pending
                        // claims on its own rebroadcast timer.
                        log::error!(
                            target: "lampo-chain",
                            "broadcast of `{txids}` failed after {BROADCAST_ATTEMPTS} attempts: {err}"
                        );
                    }
                }
            }
        });
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
    async fn brodcast_tx(&self, tx: &Transaction) -> lampo_common::error::Result<()> {
        self.backend.brodcast_tx(tx).await
    }

    async fn brodcast_txs(&self, txs: &[Transaction]) -> lampo_common::error::Result<()> {
        self.backend.brodcast_txs(txs).await
    }

    async fn fee_rate_estimation(&self, blocks: u64) -> lampo_common::error::Result<u32> {
        self.backend.fee_rate_estimation(blocks).await
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
}
