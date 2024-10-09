use std::cell::{Cell, RefCell};
use std::sync::Arc;

use lampo_common::bitcoin::BlockHash;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::ldk::chain::Listen;

use lightning_block_sync::init;
use lightning_block_sync::rpc::RpcClient;
use lightning_block_sync::{poll, AsyncBlockSourceResult, BlockHeaderData, UnboundedCache};
use lightning_block_sync::{BlockSource, SpvClient};

use lampo_common::backend::{Backend, BlockData};
use lampod::ln::{LampoChainMonitor, LampoChannel, LampoChannelManager};

/// Welcome in another Facede pattern implementation
pub struct LampoChainSync {
    config: Arc<LampoConf>,
    rpc_client: Arc<RpcClient>,
    channelmanager: RefCell<Option<Arc<LampoChannel>>>,
    chainmonitor: RefCell<Option<Arc<LampoChainMonitor>>>,
}

unsafe impl Send for LampoChainSync {}
unsafe impl Sync for LampoChainSync {}

impl Listen for LampoChainSync {
    fn block_connected(&self, block: &lampo_common::bitcoin::Block, height: u32) {
        unimplemented!()
    }

    fn block_disconnected(&self, header: &lampo_common::bitcoin::block::Header, height: u32) {
        unimplemented!()
    }

    fn filtered_block_connected(
        &self,
        header: &lampo_common::bitcoin::block::Header,
        txdata: &lampo_common::ldk::chain::transaction::TransactionData,
        height: u32,
    ) {
        unimplemented!()
    }
}

impl poll::Poll for LampoChainSync {
    fn fetch_block<'a>(
        &'a self,
        header: &'a poll::ValidatedBlockHeader,
    ) -> lightning_block_sync::AsyncBlockSourceResult<'a, poll::ValidatedBlock> {
        unimplemented!()
    }

    fn look_up_previous_header<'a>(
        &'a self,
        header: &'a poll::ValidatedBlockHeader,
    ) -> lightning_block_sync::AsyncBlockSourceResult<'a, poll::ValidatedBlockHeader> {
        unimplemented!()
    }

    fn poll_chain_tip<'a>(
        &'a self,
        best_known_chain_tip: poll::ValidatedBlockHeader,
    ) -> lightning_block_sync::AsyncBlockSourceResult<'a, poll::ChainTip> {
        unimplemented!()
    }
}

impl LampoChainSync {
    pub fn new(conf: Arc<LampoConf>) -> error::Result<Self> {
        unimplemented!()
    }

    fn channel_manager(&self) -> Arc<LampoChannel> {
        self.channelmanager.borrow().as_ref().unwrap().clone()
    }

    fn chain_monitor(&self) -> Arc<LampoChainMonitor> {
        self.chainmonitor.borrow().as_ref().unwrap().clone()
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

impl Backend for LampoChainSync {
    fn kind(&self) -> lampo_common::backend::BackendKind {
        unimplemented!()
    }

    fn brodcast_tx(&self, tx: &lampo_common::bitcoin::Transaction) {
        unimplemented!()
    }

    fn fee_rate_estimation(&self, blocks: u64) -> lampo_common::error::Result<u32> {
        unimplemented!()
    }

    // FIXME: this should be implemented by the block source no?
    fn get_best_block(
        &self,
    ) -> lampo_common::error::Result<(lampo_common::bitcoin::BlockHash, Option<u32>)> {
        unimplemented!()
    }

    // FIXME: this should be implemented by the block source no?
    fn get_block<'a>(
        &'a self,
        header_hash: &'a lampo_common::bitcoin::BlockHash,
    ) -> lampo_common::error::Result<lampo_common::backend::BlockData> {
        unimplemented!()
    }

    fn get_header<'a>(
        &'a self,
        header_hash: &'a lampo_common::bitcoin::BlockHash,
        height_hint: Option<u32>,
    ) -> lampo_common::backend::AsyncBlockSourceResult<'a, lampo_common::backend::BlockHeaderData>
    {
        unimplemented!("`get_header` is not implemented for LampoChainSync");
    }

    fn get_transaction(
        &self,
        txid: &lampo_common::bitcoin::Txid,
    ) -> lampo_common::error::Result<lampo_common::backend::TxResult> {
        unimplemented!()
    }

    fn get_utxo(
        &self,
        block: &lampo_common::bitcoin::BlockHash,
        idx: u64,
    ) -> lampo_common::backend::UtxoResult {
        unimplemented!()
    }

    fn get_utxo_by_txid(
        &self,
        txid: &lampo_common::bitcoin::Txid,
        script: &lampo_common::bitcoin::Script,
    ) -> lampo_common::error::Result<lampo_common::backend::TxResult> {
        unimplemented!()
    }

    fn is_lightway(&self) -> bool {
        unimplemented!()
    }

    fn listen(self: Arc<Self>) -> lampo_common::error::Result<()> {
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

    fn manage_transactions(
        &self,
        txs: &mut Vec<lampo_common::bitcoin::Txid>,
    ) -> lampo_common::error::Result<()> {
        unimplemented!()
    }

    fn minimum_mempool_fee(&self) -> lampo_common::error::Result<u32> {
        unimplemented!()
    }

    fn process_transactions(&self) -> lampo_common::error::Result<()> {
        unimplemented!()
    }

    fn register_output(
        &self,
        output: lampo_common::backend::WatchedOutput,
    ) -> Option<(usize, lampo_common::bitcoin::Transaction)> {
        unimplemented!()
    }

    fn set_handler(&self, _: Arc<dyn lampo_common::handler::Handler>) {
        unimplemented!()
    }

    fn watch_utxo(
        &self,
        txid: &lampo_common::bitcoin::Txid,
        script: &lampo_common::bitcoin::Script,
    ) {
        unimplemented!()
    }

    // Depending for BlockSource
}
