use std::cell::RefCell;
use std::sync::Arc;

use lampo_common::async_trait;
use lampo_common::bitcoin::BlockHash;
use lampo_common::conf::LampoConf;
use lampo_common::error;

use lightning_block_sync::http::HttpEndpoint;
use lightning_block_sync::init;
use lightning_block_sync::rpc::RpcClient;
use lightning_block_sync::{poll, AsyncBlockSourceResult, BlockHeaderData, UnboundedCache};
use lightning_block_sync::{BlockSource, SpvClient};

use lampo_common::backend::{Backend, BlockData};
use lampo_common::types::{LampoChainMonitor, LampoChannel};

/// Welcome in another Facede pattern implementation
pub struct LampoChainSync {
    config: Arc<LampoConf>,
    rpc_client: Arc<RpcClient>,
    channel_manager: RefCell<Option<Arc<LampoChannel>>>,
    chain_monitor: RefCell<Option<Arc<LampoChainMonitor>>>,
    handler: RefCell<Option<Arc<dyn lampo_common::handler::Handler>>>,
}

unsafe impl Send for LampoChainSync {}
unsafe impl Sync for LampoChainSync {}

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

        // FIXME: somehow we should fix this
        let url_parts: Vec<&str> = core_url.split(':').collect();
        let host = url_parts[1];
        let host = host.strip_prefix("//").unwrap_or(host);
        let port = url_parts[2].parse::<u16>()?;

        log::debug!("Connecting to core at: {:?} - {host}", url_parts);

        let http_endpoint = HttpEndpoint::for_host(host.to_owned()).with_port(port);
        let rpc_credentials = base64::encode(format!("{}:{}", core_user, core_pass));

        let rpc = RpcClient::new(&rpc_credentials, http_endpoint)?;

        Ok(Self {
            config: conf,
            rpc_client: Arc::new(rpc),
            channel_manager: RefCell::new(None),
            chain_monitor: RefCell::new(None),
            handler: RefCell::new(None),
        })
    }

    pub fn set_channel_manager(&self, channel_manager: Arc<LampoChannel>) {
        self.channel_manager.borrow_mut().replace(channel_manager);
    }

    pub fn set_chain_monitor(&self, chain_monitor: Arc<LampoChainMonitor>) {
        self.chain_monitor.borrow_mut().replace(chain_monitor);
    }

    fn channel_manager(&self) -> Arc<LampoChannel> {
        self.channel_manager.borrow().as_ref().unwrap().clone()
    }

    fn chain_monitor(&self) -> Arc<LampoChainMonitor> {
        self.chain_monitor.borrow().as_ref().unwrap().clone()
    }
}

impl BlockSource for LampoChainSync {
    fn get_header<'a>(
        &'a self,
        header_hash: &'a BlockHash,
        height_hint: Option<u32>,
    ) -> AsyncBlockSourceResult<'a, BlockHeaderData> {
        Box::pin(async move { self.rpc_client.get_header(header_hash, height_hint).await })
    }

    fn get_block<'a>(
        &'a self,
        header_hash: &'a BlockHash,
    ) -> AsyncBlockSourceResult<'a, BlockData> {
        Box::pin(async move { self.rpc_client.get_block(header_hash).await })
    }

    fn get_best_block<'a>(&'a self) -> AsyncBlockSourceResult<(BlockHash, Option<u32>)> {
        Box::pin(async move { self.rpc_client.get_best_block().await })
    }
}

#[async_trait]
impl Backend for LampoChainSync {
    fn kind(&self) -> lampo_common::backend::BackendKind {
        lampo_common::backend::BackendKind::Core
    }

    async fn brodcast_tx(&self, tx: &lampo_common::bitcoin::Transaction) {
        unimplemented!()
    }

    async fn fee_rate_estimation(&self, blocks: u64) -> lampo_common::error::Result<u32> {
        Ok(256)
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
        unimplemented!()
    }

    fn set_handler(&self, handler: Arc<dyn lampo_common::handler::Handler>) {
        self.handler.borrow_mut().replace(handler);
    }

    fn set_channel_manager(&self, channel_manager: Arc<LampoChannel>) {
        self.set_channel_manager(channel_manager);
    }

    fn set_chain_monitor(&self, chain_monitor: Arc<LampoChainMonitor>) {
        self.set_chain_monitor(chain_monitor);
    }

    async fn listen(self: Arc<Self>) -> lampo_common::error::Result<()> {
        tokio::spawn(async move {
            let mut cache = UnboundedCache::new();
            let chain_poller = poll::ChainPoller::new(self.as_ref(), self.config.network);
            let chain_listener = (self.chain_monitor(), self.channel_manager());

            let polled_chain_tip = init::validate_best_block_header(self.as_ref())
                .await
                .unwrap();

            // FIXME: we should look at how we do
            let mut spv_client =
                SpvClient::new(polled_chain_tip, chain_poller, &mut cache, &chain_listener);
            loop {
                spv_client.poll_best_tip().await.unwrap();
            }
        });
        Ok(())
    }
}
