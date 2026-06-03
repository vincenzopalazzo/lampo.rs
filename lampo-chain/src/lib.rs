use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use lampo_common::event::onchain::OnChainEvent;
use lampo_common::event::Event;
use lightning_block_sync::init;
use lightning_block_sync::rpc::RpcClient;
use lightning_block_sync::{poll, BlockHeaderData, BlockSourceResult};
use lightning_block_sync::{BlockSource, SpvClient};

use lampo_common::async_trait;
use lampo_common::backend::{Backend, BlockData};
use lampo_common::bitcoin::consensus::encode::serialize_hex;
use lampo_common::bitcoin::{Block, BlockHash};
use lampo_common::chainsync::ChainSyncCoordinator;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::json;
use lampo_common::ldk::chain;
use lampo_common::serde::Deserialize;
use lampo_common::types::{LampoChainMonitor, LampoChannel};
use lampo_common::wallet::WalletManager;

/// Adapts the on-chain wallet to LDK's [`chain::Listen`] so it can ride the
/// same `synchronize_listeners` pass as the channel manager and chain monitor
/// -- one RPC stream for the whole node, instead of a second `getblock` scan.
///
/// This is the *only* place the LDK chain-sync types meet the wallet: the
/// coupling is confined to the bitcoind backend crate. The wallet itself
/// stays free of LDK, driven through the lampo-native `WalletManager` API.
struct WalletChainListener {
    wallet: Arc<dyn WalletManager>,
    /// Set if any `apply_block` failed during a sync pass. `Listen` methods
    /// can't return errors, so we record the failure here for the caller to
    /// surface instead of silently advertising a successful unified sync.
    failed: AtomicBool,
}

impl WalletChainListener {
    fn new(wallet: Arc<dyn WalletManager>) -> Self {
        Self {
            wallet,
            failed: AtomicBool::new(false),
        }
    }

    /// Clear the failure flag before a (re)try of `synchronize_listeners`.
    fn reset(&self) {
        self.failed.store(false, Ordering::Relaxed);
    }

    /// Whether any block apply failed during the last sync pass.
    fn had_failure(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

impl chain::Listen for WalletChainListener {
    fn filtered_block_connected(
        &self,
        _header: &lampo_common::bitcoin::block::Header,
        _txdata: &chain::transaction::TransactionData,
        _height: u32,
    ) {
        // Lampo's bitcoind backend delivers full blocks, so `block_connected`
        // below is what actually runs. Mirrors ldk-node's wallet listener.
        debug_assert!(
            false,
            "filtered_block_connected is unsupported for the on-chain wallet"
        );
    }

    fn block_connected(&self, block: &Block, height: u32) {
        if let Err(err) = self.wallet.apply_block(block, height) {
            log::error!(
                target: "lampo-chain",
                "on-chain wallet apply_block at height {height} failed: {err}"
            );
            self.failed.store(true, Ordering::Relaxed);
        }
    }

    fn blocks_disconnected(&self, _fork_point_block: chain::BlockLocator) {
        // BDK rolls the wallet chain back via the next `block_connected`'s
        // `connected_to` (the new parent), so an explicit disconnect is a
        // no-op. Reorgs do not occur during the initial historical catch-up;
        // ongoing reorgs are handled by the wallet's Emitter poll.
        log::debug!(target: "lampo-chain", "on-chain wallet listener: blocks_disconnected");
    }
}

/// Welcome in another Facede pattern implementation
pub struct LampoChainSync {
    config: Arc<LampoConf>,
    rpc_client: Arc<RpcClient>,
    channel_manager: OnceLock<Arc<LampoChannel>>,
    chain_monitor: OnceLock<Arc<LampoChainMonitor>>,
    handler: OnceLock<Arc<dyn lampo_common::handler::Handler>>,
    coordinator: OnceLock<Arc<ChainSyncCoordinator>>,
    wallet: OnceLock<Arc<dyn WalletManager>>,
}

impl LampoChainSync {
    pub fn new(conf: Arc<LampoConf>) -> error::Result<Self> {
        let core_url = conf.core_url.as_ref().ok_or(error::anyhow!(
            "Core URL is missing from the configuration file"
        ))?;
        let core_user = conf.core_user.as_ref().ok_or(error::anyhow!(
            "Core User is missing from the configuration file"
        ))?;
        let core_pass = conf.core_pass.as_ref().ok_or(error::anyhow!(
            "Core Password is missing from the configuration file"
        ))?;

        log::debug!("Core URL: {:?}", core_url);
        // FIXME: somehow we should fix this
        let url_parts: Vec<&str> = core_url.split(':').collect();
        let host = url_parts[1];
        let host = host.strip_prefix("//").unwrap_or(host);
        let port = url_parts[2].parse::<u16>()?;

        log::debug!("Connecting to core at: {:?} - {host}", url_parts);

        let base_url = format!("http://{host}:{port}");
        let rpc_credentials = base64::encode(format!("{}:{}", core_user, core_pass));

        let rpc = RpcClient::new(&rpc_credentials, base_url);

        Ok(Self {
            config: conf,
            rpc_client: Arc::new(rpc),
            channel_manager: OnceLock::new(),
            chain_monitor: OnceLock::new(),
            handler: OnceLock::new(),
            coordinator: OnceLock::new(),
            wallet: OnceLock::new(),
        })
    }

    pub fn set_channel_manager(&self, channel_manager: Arc<LampoChannel>) {
        self.channel_manager
            .set(channel_manager)
            .unwrap_or_else(|_| panic!("channel manager already set"));
    }

    pub fn set_chain_monitor(&self, chain_monitor: Arc<LampoChainMonitor>) {
        self.chain_monitor
            .set(chain_monitor)
            .unwrap_or_else(|_| panic!("chain monitor already set"));
    }

    fn channel_manager(&self) -> Arc<LampoChannel> {
        self.channel_manager
            .get()
            .expect("channel manager not set")
            .clone()
    }

    fn chain_monitor(&self) -> Arc<LampoChainMonitor> {
        self.chain_monitor
            .get()
            .expect("chain monitor not set")
            .clone()
    }

    fn wallet(&self) -> Option<Arc<dyn WalletManager>> {
        self.wallet.get().cloned()
    }
}

impl BlockSource for LampoChainSync {
    fn get_header<'a>(
        &'a self,
        header_hash: &'a BlockHash,
        height_hint: Option<u32>,
    ) -> impl std::future::Future<Output = BlockSourceResult<BlockHeaderData>> + Send + 'a {
        async move { self.rpc_client.get_header(header_hash, height_hint).await }
    }

    fn get_block<'a>(
        &'a self,
        header_hash: &'a BlockHash,
    ) -> impl std::future::Future<Output = BlockSourceResult<BlockData>> + Send + 'a {
        async move { self.rpc_client.get_block(header_hash).await }
    }

    fn get_best_block<'a>(
        &'a self,
    ) -> impl std::future::Future<Output = BlockSourceResult<(BlockHash, Option<u32>)>> + Send + 'a
    {
        async move { self.rpc_client.get_best_block().await }
    }
}

#[async_trait]
impl Backend for LampoChainSync {
    fn kind(&self) -> lampo_common::backend::BackendKind {
        lampo_common::backend::BackendKind::Core
    }

    async fn get_best_block(&self) -> BlockSourceResult<(BlockHash, Option<u32>)> {
        self.rpc_client.get_best_block().await
    }

    async fn brodcast_tx(&self, tx: &lampo_common::bitcoin::Transaction) {
        let resp = self
            .rpc_client
            .call_method::<json::Value>("sendrawtransaction", &[serialize_hex(tx).into()])
            .await;
        log::info!("Broadcasting tx result: {:?}", resp);
        if resp.is_ok() {
            let Some(handler) = self.handler.get() else {
                return;
            };
            handler.emit(Event::OnChain(OnChainEvent::SendRawTransaction(tx.clone())));
        }
        // FIXME: emit the brodcast event for lampo in case of errors, just to unlock the client
    }

    async fn fee_rate_estimation(&self, blocks: u64) -> lampo_common::error::Result<u32> {
        #[derive(Deserialize)]
        pub struct FeeRate {
            feerate: Option<f64>,
            errors: Option<Vec<String>>,
        }

        if self.config.network == lampo_common::bitcoin::Network::Regtest {
            return Ok(256);
        }

        let resp = self
            .rpc_client
            .call_method::<json::Value>("estimatesmartfee", &[blocks.into()])
            .await?;
        let resp: FeeRate = json::from_value(resp)?;
        if let Some(errs) = resp.errors {
            return Err(error::anyhow!("Error in fee rate estimation: {:?}", errs).into());
        }
        let Some(feerate) = resp.feerate else {
            return Err(error::anyhow!("No fee rate estimation available").into());
        };
        // estimate fee rate in BTC/kvB
        Ok((feerate * (100_000_000 as f64)) as u32)
    }

    async fn get_transaction(
        &self,
        txid: &lampo_common::bitcoin::Txid,
    ) -> lampo_common::error::Result<lampo_common::backend::TxResult> {
        unimplemented!()
    }

    async fn get_utxo(
        &self,
        block: &lampo_common::bitcoin::BlockHash,
        idx: u64,
    ) -> lampo_common::backend::UtxoResult {
        unimplemented!()
    }

    async fn get_utxo_by_txid(
        &self,
        txid: &lampo_common::bitcoin::Txid,
        script: &lampo_common::bitcoin::Script,
    ) -> lampo_common::error::Result<lampo_common::backend::TxResult> {
        unimplemented!()
    }

    // TODO: specify what kind of format the result should be!
    async fn minimum_mempool_fee(&self) -> lampo_common::error::Result<u32> {
        #[derive(Debug, Deserialize)]
        struct MempoolInfo {
            loaded: bool,
            mempoolminfee: f64,
        };
        let mempool_info = self
            .rpc_client
            .call_method::<json::Value>("getmempoolinfo", &[])
            .await?;
        let mempool_info: MempoolInfo = json::from_value(mempool_info)?;
        if mempool_info.loaded {
            log::warn!("mempool is still loading, so the fee may be not accurate!");
        }
        Ok((mempool_info.mempoolminfee * (100_000_000 as f64)) as u32)
    }

    fn set_handler(&self, handler: Arc<dyn lampo_common::handler::Handler>) {
        self.handler
            .set(handler)
            .unwrap_or_else(|_| panic!("backend handler already set"));
    }

    fn set_channel_manager(&self, channel_manager: Arc<LampoChannel>) {
        self.set_channel_manager(channel_manager);
    }

    fn set_chain_monitor(&self, chain_monitor: Arc<LampoChainMonitor>) {
        self.set_chain_monitor(chain_monitor);
    }

    fn set_coordinator(&self, coordinator: Arc<ChainSyncCoordinator>) {
        self.coordinator
            .set(coordinator)
            .unwrap_or_else(|_| panic!("chain sync coordinator already set"));
    }

    fn set_wallet_manager(&self, wallet: Arc<dyn WalletManager>) {
        self.wallet
            .set(wallet)
            .unwrap_or_else(|_| panic!("wallet manager already set"));
    }

    async fn listen(self: Arc<Self>) -> lampo_common::error::Result<()> {
        let channel_manager = self.channel_manager();
        let chain_monitor = self.chain_monitor();

        // Synchronize the channel manager and chain monitor from their
        // persisted best block up to the current chain tip. This is critical
        // on restart: the ChannelManager may have been persisted at block N,
        // but the chain may now be at block N+M. Without this sync, the
        // SpvClient would start at the current tip and try to connect block
        // N+M+1 to the ChannelManager which still thinks it's at block N,
        // causing a "Blocks must be connected in chain-order" assertion.
        let manager_best = channel_manager.current_best_block();

        // Include the on-chain wallet in the same sync pass so one RPC stream
        // catches up the channel manager, chain monitor, and wallet together --
        // deduplicating the overlapping block range instead of a second
        // `getblock` scan. Excluded in `legacy` sync mode, and when
        // `wallet_sync_parallel` is set (the wallet then runs its own Emitter,
        // so attaching it here too would double-apply blocks). Kept in a local
        // so it outlives the `chain_listeners` vec consumed on each attempt.
        let parallel = self.config.wallet_sync_parallel.unwrap_or(false);
        let legacy = self
            .config
            .sync_mode
            .as_deref()
            .map(|mode| mode.eq_ignore_ascii_case("legacy"))
            .unwrap_or(false);
        let unified = !legacy && !parallel;
        let wallet = if unified { self.wallet() } else { None };
        let wallet_listener = wallet.as_ref().map(|w| WalletChainListener::new(w.clone()));

        log::info!(
            target: "lampo-chain",
            "Syncing chain listeners from block {} (height {}) to current tip",
            manager_best.block_hash,
            manager_best.height
        );

        // Retry on transient RPC failures so a hiccup in `synchronize_listeners`
        // can't leave the coordinator stuck in `PendingInitialSync` (which would
        // permanently gate the on-chain wallet). Each attempt rebuilds the
        // listener set from the current best blocks, resuming where it left off.
        let (cache, synced_chain_tip) = loop {
            if let Some(listener) = wallet_listener.as_ref() {
                listener.reset();
            }
            let manager_best = channel_manager.current_best_block();
            let mut chain_listeners: Vec<(
                chain::BlockLocator,
                &(dyn chain::Listen + Send + Sync),
            )> = vec![
                (
                    manager_best.clone(),
                    &*channel_manager as &(dyn chain::Listen + Send + Sync),
                ),
                (
                    manager_best.clone(),
                    &*chain_monitor as &(dyn chain::Listen + Send + Sync),
                ),
            ];
            if let (Some(wallet), Some(listener)) = (wallet.as_ref(), wallet_listener.as_ref()) {
                match wallet.current_best_block() {
                    Ok(best) => {
                        log::info!(
                            target: "lampo-chain",
                            "Including on-chain wallet in chain sync from height {}",
                            best.height
                        );
                        chain_listeners.push((
                            chain::BlockLocator::new(best.hash, best.height),
                            listener as &(dyn chain::Listen + Send + Sync),
                        ));
                    }
                    Err(err) => {
                        log::error!(target: "lampo-chain", "skipping on-chain wallet in chain sync: {err}")
                    }
                }
            }

            match init::synchronize_listeners(self.as_ref(), self.config.network, chain_listeners)
                .await
            {
                Ok(result) => break result,
                Err(e) => {
                    log::error!(
                        target: "lampo-chain",
                        "Failed to synchronize chain listeners, retrying in 5s: {:?}", e
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        };

        log::info!(target: "lampo-chain", "Chain listeners synced to current tip");

        // If the wallet failed to apply some blocks during the pass, surface it.
        // The node still advances the LDK listeners; the gated wallet Emitter
        // will recover the wallet from its last good checkpoint.
        if wallet_listener.as_ref().is_some_and(|l| l.had_failure()) {
            log::warn!(
                target: "lampo-chain",
                "on-chain wallet did not fully apply during unified sync; the wallet scan will recover it from its last good checkpoint"
            );
        }

        // Publish listener-sync completion so gated components (e.g. the
        // on-chain wallet) can proceed over the now-free RPC. No-op when no
        // coordinator was injected.
        if let Some(coordinator) = self.coordinator.get() {
            coordinator.mark_listeners_synced();
        }

        let chain_listener = (chain_monitor, channel_manager);
        let chain_poller = poll::ChainPoller::new(self.as_ref(), self.config.network);
        let mut spv_client = SpvClient::new(synced_chain_tip, chain_poller, cache, &chain_listener);
        log::info!(target: "lampo-chain", "Start Backend ...");
        loop {
            if let Err(err) = spv_client.poll_best_tip().await {
                log::error!(target: "lampo-chain", "Error while polling best tip: {:?}", err);
            }
            // FIXME: make this configurable
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}
