use std::sync::Arc;

use lampo_bdk_wallet::BDKWalletManager;
use lampo_chain::LampoChainSync;
use lampo_common::backend::{AsyncBlockSourceResult, Backend, BlockData, BlockHeaderData};
use lampo_common::bitcoin::absolute::Height;
use lampo_common::bitcoin::{Amount, BlockHash, FeeRate, Script, ScriptBuf, Transaction};
use lampo_common::conf::LampoConf;
use lampo_common::keys::LampoKeys;
use lampo_common::ldk::block_sync::BlockSource;
use lampo_common::model::response::{NewAddress, Utxo};
use lampo_common::types::{LampoChainMonitor, LampoChannel};
use lampo_common::wallet::WalletManager;
use lampo_common::{async_trait, error};

pub struct LampoArkWallet {
    pub inner: BDKWalletManager,
    pub backend: LampoChainSync,
}

#[async_trait]
impl WalletManager for LampoArkWallet {
    async fn new(conf: Arc<LampoConf>) -> error::Result<(Self, String)> {
        let (wallet, mnemonic_words) = BDKWalletManager::new(conf.clone()).await?;
        let backend = LampoChainSync::new(conf.clone())?;
        Ok((
            Self {
                inner: wallet,
                backend,
            },
            mnemonic_words,
        ))
    }

    async fn restore(conf: Arc<LampoConf>, mnemonic_words: &str) -> error::Result<Self> {
        let wallet = BDKWalletManager::restore(conf.clone(), mnemonic_words).await?;
        let backend = LampoChainSync::new(conf.clone())?;
        Ok(Self {
            inner: wallet,
            backend,
        })
    }

    fn ldk_keys(&self) -> Arc<LampoKeys> {
        self.inner.ldk_keys()
    }

    async fn get_onchain_address(&self) -> error::Result<NewAddress> {
        self.inner.get_onchain_address().await
    }

    async fn get_onchain_balance(&self) -> error::Result<u64> {
        self.inner.get_onchain_balance().await
    }

    async fn create_transaction(
        &self,
        script: ScriptBuf,
        amount: Amount,
        fee_rate: FeeRate,
        best_block: Height,
    ) -> error::Result<Transaction> {
        // FIXME: this need to build a magic ARK funding transaction
        self.inner
            .create_transaction(script, amount, fee_rate, best_block)
            .await
    }

    async fn list_transactions(&self) -> error::Result<Vec<Utxo>> {
        self.inner.list_transactions().await
    }

    async fn sync(&self) -> error::Result<()> {
        self.inner.sync().await
    }

    async fn wallet_tips(&self) -> error::Result<Height> {
        self.inner.wallet_tips().await
    }

    async fn listen(self: Arc<Self>) -> error::Result<()> {
        self.inner.listen().await
    }
}

#[async_trait]
impl Backend for LampoArkWallet {
    fn kind(&self) -> lampo_common::backend::BackendKind {
        lampo_common::backend::BackendKind::Core
    }

    async fn brodcast_tx(&self, tx: &Transaction) {
        self.backend.brodcast_tx(tx).await;
    }

    async fn fee_rate_estimation(&self, blocks: u64) -> error::Result<u32> {
        self.backend.fee_rate_estimation(blocks).await
    }

    async fn get_transaction(&self, txid: &str) -> error::Result<Transaction> {
        unimplemented!("This is not needed with the current LDK interface")
    }

    async fn get_utxo(&self, txid: &str, vout: u32) -> error::Result<Utxo> {
        unimplemented!("This is not needed with the current LDK interface")
    }

    async fn get_utxo_by_txid(
        &self,
        txid: &lampo_common::bitcoin::Txid,
        script: &lampo_common::bitcoin::Script,
    ) -> lampo_common::error::Result<lampo_common::backend::TxResult> {
        unimplemented!()
    }

    async fn minimum_mempool_fee(&self) -> lampo_common::error::Result<u32> {
        self.backend.minimum_mempool_fee().await
    }

    fn set_handler(&self, handler: Arc<dyn lampo_common::handler::Handler>) {
        self.backend.set_handler(handler);
    }

    fn set_channel_manager(&self, channel_manager: Arc<LampoChannel>) {
        self.backend.set_channel_manager(channel_manager);
    }

    fn set_chain_monitor(&self, chain_monitor: Arc<LampoChainMonitor>) {
        self.backend.set_chain_monitor(chain_monitor);
    }

    async fn listen(self: Arc<Self>) -> lampo_common::error::Result<()> {
        self.backend.listen().await
    }
}

impl BlockSource for LampoArkWallet {
    fn get_header<'a>(
        &'a self,
        header_hash: &'a BlockHash,
        height_hint: Option<u32>,
    ) -> AsyncBlockSourceResult<'a, BlockHeaderData> {
        self.backend.get_header(header_hash, height_hint)
    }

    fn get_block<'a>(
        &'a self,
        header_hash: &'a BlockHash,
    ) -> AsyncBlockSourceResult<'a, BlockData> {
        self.backend.get_block(header_hash)
    }

    fn get_best_block<'a>(&'a self) -> AsyncBlockSourceResult<(BlockHash, Option<u32>)> {
        self.backend.get_best_block()
    }
}
