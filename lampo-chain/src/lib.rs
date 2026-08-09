use std::sync::{Arc, Mutex, OnceLock};

use lampo_common::event::onchain::OnChainEvent;
use lampo_common::event::Event;
use lightning_block_sync::init;
use lightning_block_sync::poll::ValidatedBlockHeader;
use lightning_block_sync::rpc::RpcClient;
use lightning_block_sync::HeaderCache;
use lightning_block_sync::{poll, BlockHeaderData, BlockSourceResult};
use lightning_block_sync::{BlockSource, SpvClient};

use lampo_common::async_trait;
use lampo_common::backend::{Backend, BlockData};
use lampo_common::bitcoin::consensus::encode::serialize_hex;
use lampo_common::bitcoin::BlockHash;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::json;
use lampo_common::ldk::chain;
use lampo_common::ldk::chain::chaininterface::FEERATE_FLOOR_SATS_PER_KW;
use lampo_common::ldk::chain::{BlockLocator, ChannelMonitorUpdateStatus, Watch};
use lampo_common::serde::Deserialize;
use lampo_common::types::{LampoChainMonitor, LampoChannel, LampoMonitorListener};

/// Welcome in another Facede pattern implementation
pub struct LampoChainSync {
    config: Arc<LampoConf>,
    rpc_client: Arc<RpcClient>,
    channel_manager: OnceLock<Arc<LampoChannel>>,
    chain_monitor: OnceLock<Arc<LampoChainMonitor>>,
    handler: OnceLock<Arc<dyn lampo_common::handler::Handler>>,
    /// Channel monitors read from disk on restart. Each is synced to the
    /// chain tip from its own best block during [`Backend::sync_chain`] and
    /// then registered with the chain monitor via `watch_channel`.
    stale_monitors: Mutex<Vec<(BlockLocator, LampoMonitorListener)>>,
    /// Header cache and validated chain tip produced by the initial sync,
    /// consumed by [`Backend::listen`] to seed the SPV client.
    sync_state: Mutex<Option<(HeaderCache, ValidatedBlockHeader)>>,
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
            stale_monitors: Mutex::new(Vec::new()),
            sync_state: Mutex::new(None),
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

    fn emit_broadcast_success(&self, tx: &lampo_common::bitcoin::Transaction) {
        if let Some(handler) = self.handler.get() {
            handler.emit(Event::OnChain(OnChainEvent::SendRawTransaction(tx.clone())));
        }
    }

    fn emit_broadcast_failure(&self, tx: &lampo_common::bitcoin::Transaction) {
        if let Some(handler) = self.handler.get() {
            handler.emit(Event::OnChain(OnChainEvent::BroadcastFailed(
                tx.compute_txid(),
            )));
        }
    }
}

/// Whether a bitcoind broadcast error means the transaction (or package)
/// is already in the mempool or the chain, i.e. the broadcast goal is met.
fn is_already_broadcast_error(err: &str) -> bool {
    err.contains("already in block chain")
        || err.contains("txn-already-in-mempool")
        || err.contains("txn-already-known")
        || err.contains("already known")
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

    async fn brodcast_tx(
        &self,
        tx: &lampo_common::bitcoin::Transaction,
    ) -> lampo_common::error::Result<()> {
        let resp = self
            .rpc_client
            .call_method::<json::Value>("sendrawtransaction", &[serialize_hex(tx).into()])
            .await;
        log::debug!(target: "lampo-chain", "broadcasting tx `{}` result: {:?}", tx.compute_txid(), resp);
        match resp {
            Ok(_) => {
                self.emit_broadcast_success(tx);
                Ok(())
            }
            // LDK's rebroadcast timer resends transactions that may already
            // be in the mempool or confirmed; bitcoind rejects those with
            // "already known"-style errors that mean the broadcast goal is
            // achieved, not failed.
            Err(err) if is_already_broadcast_error(&format!("{err:?}")) => {
                self.emit_broadcast_success(tx);
                Ok(())
            }
            Err(err) => {
                self.emit_broadcast_failure(tx);
                Err(error::anyhow!(
                    "failed to broadcast tx `{}`: {:?}",
                    tx.compute_txid(),
                    err
                ))
            }
        }
    }

    async fn brodcast_txs(
        &self,
        txs: &[lampo_common::bitcoin::Transaction],
    ) -> lampo_common::error::Result<()> {
        let [_, _, ..] = txs else {
            // Zero or one transaction: no package semantics needed.
            if let Some(tx) = txs.first() {
                return self.brodcast_tx(tx).await;
            }
            return Ok(());
        };
        // More than one transaction is a package (a child and its
        // parents, e.g. an anchor CPFP paying for a low-feerate
        // commitment): it must be submitted atomically or the parent can
        // be rejected for paying below the mempool minimum feerate.
        let hexes = txs
            .iter()
            .map(|tx| json::Value::from(serialize_hex(tx)))
            .collect::<Vec<_>>();
        let resp = self
            .rpc_client
            .call_method::<json::Value>("submitpackage", &[json::Value::from(hexes)])
            .await;
        log::debug!(target: "lampo-chain", "submitpackage result: {:?}", resp);
        let err = match resp {
            Ok(resp) => {
                if resp.get("package_msg").and_then(|msg| msg.as_str()) == Some("success") {
                    for tx in txs {
                        self.emit_broadcast_success(tx);
                    }
                    return Ok(());
                }
                error::anyhow!("submitpackage rejected the package: {resp}")
            }
            Err(err) if is_already_broadcast_error(&format!("{err:?}")) => {
                for tx in txs {
                    self.emit_broadcast_success(tx);
                }
                return Ok(());
            }
            Err(err) => error::anyhow!("submitpackage failed: {err:?}"),
        };
        for tx in txs {
            self.emit_broadcast_failure(tx);
        }
        Err(err)
    }

    async fn fee_rate_estimation(&self, blocks: u64) -> lampo_common::error::Result<u32> {
        #[derive(Deserialize)]
        pub struct FeeRate {
            feerate: Option<f64>,
            errors: Option<Vec<String>>,
        }

        if self.config.network == lampo_common::bitcoin::Network::Regtest {
            return Ok(FEERATE_FLOOR_SATS_PER_KW);
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
        // `estimatesmartfee` returns BTC/kvB; convert to sat/kvB and then to
        // sats per 1000 weight units (a vbyte is 4 weight units).
        let sat_per_kvb = feerate * 100_000_000f64;
        Ok(((sat_per_kvb / 4.0) as u32).max(FEERATE_FLOOR_SATS_PER_KW))
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
        if !mempool_info.loaded {
            log::warn!("mempool is still loading, so the fee may be not accurate!");
        }
        // `mempoolminfee` is in BTC/kvB; convert to sats per 1000 weight units.
        let sat_per_kvb = mempool_info.mempoolminfee * 100_000_000f64;
        Ok(((sat_per_kvb / 4.0) as u32).max(FEERATE_FLOOR_SATS_PER_KW))
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

    fn set_stale_monitors(&self, monitors: Vec<(BlockLocator, LampoMonitorListener)>) {
        *self.stale_monitors.lock().unwrap() = monitors;
    }

    async fn sync_chain(&self) -> lampo_common::error::Result<()> {
        let channel_manager = self.channel_manager();
        let chain_monitor = self.chain_monitor();

        // Synchronize the channel manager, the chain monitor, and every
        // channel monitor read from disk, each from its own persisted best
        // block up to the current chain tip. This is critical on restart:
        // the ChannelManager may have been persisted at block N while a
        // ChannelMonitor was persisted at block N-M; each listener gets
        // exactly the blocks it is missing, so no on-chain HTLC claim or
        // counterparty revocation in that window can be skipped.
        let stale_monitors = std::mem::take(&mut *self.stale_monitors.lock().unwrap());
        let manager_best = channel_manager.current_best_block();
        let mut chain_listeners: Vec<(chain::BlockLocator, &(dyn chain::Listen + Send + Sync))> = vec![
            (
                manager_best.clone(),
                &*channel_manager as &(dyn chain::Listen + Send + Sync),
            ),
            // On restart the chain monitor holds no monitors yet (they are
            // registered below, after they synced individually), so the
            // manager's best block is a valid starting point for it.
            (
                manager_best.clone(),
                &*chain_monitor as &(dyn chain::Listen + Send + Sync),
            ),
        ];
        for (locator, listener) in &stale_monitors {
            log::info!(
                target: "lampo-chain",
                "Syncing channel monitor `{}` from height {} to current tip",
                listener.0.channel_id(),
                locator.height,
            );
            chain_listeners.push((
                locator.clone(),
                listener as &(dyn chain::Listen + Send + Sync),
            ));
        }

        log::info!(
            target: "lampo-chain",
            "Syncing chain listeners from block {} (height {}) to current tip",
            manager_best.block_hash,
            manager_best.height
        );

        let (cache, synced_chain_tip) =
            init::synchronize_listeners(self, self.config.network, chain_listeners)
                .await
                .map_err(|e| error::anyhow!("Failed to synchronize chain listeners: {:?}", e))?;

        log::info!(target: "lampo-chain", "Chain listeners synced to current tip");

        // Now that every monitor is at the chain tip, hand it to the chain
        // monitor so it keeps watching the channel from here on. Failing to
        // register a monitor means force-closes and HTLC claims for that
        // channel would go undetected: abort startup instead.
        for (_, (monitor, ..)) in stale_monitors {
            let channel_id = monitor.channel_id();
            match chain_monitor.watch_channel(channel_id, monitor) {
                Ok(ChannelMonitorUpdateStatus::Completed)
                | Ok(ChannelMonitorUpdateStatus::InProgress) => {
                    log::info!(target: "lampo-chain", "Watching channel `{channel_id}`");
                }
                Ok(ChannelMonitorUpdateStatus::UnrecoverableError) | Err(()) => {
                    error::bail!(
                        "failed to register channel monitor `{channel_id}` with the chain monitor"
                    );
                }
            }
        }

        *self.sync_state.lock().unwrap() = Some((cache, synced_chain_tip));
        Ok(())
    }

    async fn listen(self: Arc<Self>) -> lampo_common::error::Result<()> {
        // If the caller did not run `sync_chain` yet, do it now: the SPV
        // client below must start from a synced tip.
        if self.sync_state.lock().unwrap().is_none() {
            self.sync_chain().await?;
        }
        let (cache, synced_chain_tip) = self
            .sync_state
            .lock()
            .unwrap()
            .take()
            .expect("sync_chain populated the state above");

        let chain_listener = (self.chain_monitor(), self.channel_manager());
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
