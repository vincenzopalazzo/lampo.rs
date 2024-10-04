use std::cell::{Cell, RefCell};
use std::sync::Arc;

use lampo_common::{
    error::{self, Ok},
    ldk::chain::Confirm,
};

use lightning_transaction_sync::EsploraSyncClient;

use lampo_common::{
    backend::{Backend, BlockData},
    utils::logger::LampoLogger,
};
use lampod::ln::{LampoChainMonitor, LampoChannel, LampoChannelManager};

/// Welcome in another Facede pattern implementation
pub struct LampoChainSync {
    chain: EsploraSyncClient<Arc<LampoLogger>>,

    channelmanager: RefCell<Option<Arc<LampoChannel>>>,
    chainmonitor: RefCell<Option<Arc<LampoChainMonitor>>>,
}

impl LampoChainSync {
    fn channel_manager(&self) -> Arc<LampoChannel> {
        self.channelmanager.borrow().as_ref().unwrap().clone()
    }

    fn chain_monitor(&self) -> Arc<LampoChainMonitor> {
        self.chainmonitor.borrow().as_ref().unwrap().clone()
    }
}

impl Backend for LampoChainSync {
    fn kind(&self) -> lampo_common::backend::BackendKind {
        unimplemented!()
    }

    fn brodcast_tx(&self, tx: &lampo_common::bitcoin::Transaction) {
        let Err(err) = self.chain.client().broadcast(tx) else {
            return;
        };
        log::error!("Error broadcasting tx: {}", err);
    }

    fn fee_rate_estimation(&self, blocks: u64) -> lampo_common::error::Result<u32> {
        let feerate = self
            .chain
            .client()
            .get_fee_estimates()
            .map(|fees| fees.get(&format!("{blocks}")).map_or(0, |fee| *fee as u32))?;
        Ok(feerate)
    }

    fn get_best_block(
        &self,
    ) -> lampo_common::error::Result<(lampo_common::bitcoin::BlockHash, Option<u32>)> {
        unimplemented!()
    }

    fn get_block<'a>(
        &'a self,
        header_hash: &'a lampo_common::bitcoin::BlockHash,
    ) -> lampo_common::error::Result<lampo_common::backend::BlockData> {
        let block = self.chain.client().get_block_by_hash(&header_hash)?;
        let Some(block) = block else {
            error::bail!("Block `{}` not found", header_hash.to_string());
        };
        Ok(BlockData::FullBlock(block))
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
        let tx = self.chain.client().get_tx(&txid)?;
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

    fn listen(self: Arc<Self>) -> lampo_common::error::Result<std::thread::JoinHandle<()>> {
        // FIXME: we should set a timer
        loop {
            // FIXME: add the chain monitor in here and the channel manager
            self.chain.sync(vec![
                self.chain_monitor().as_ref() as &(dyn Confirm + Send + std::marker::Sync),
                self.channel_manager().as_ref() as &(dyn Confirm + Send + std::marker::Sync),
            ])?;
        }
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
}
