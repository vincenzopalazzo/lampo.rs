use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;

use lampo_bdk_wallet::BDKWalletManager;
use lampo_chain::LampoChainSync;
use lampo_common::backend::BackendKind;
use lampo_common::backend::TxResult;
use lampo_common::backend::{
    AsyncBlockSourceResult, Backend, BlockData, BlockHeaderData, WatchedOutput,
};
use lampo_common::bitcoin::absolute::Height;
use lampo_common::bitcoin::{Amount, BlockHash, FeeRate, Script, ScriptBuf, Transaction, Txid};
use lampo_common::conf::LampoConf;
use lampo_common::keys::LampoKeys;
use lampo_common::ldk::block_sync::BlockSource;
use lampo_common::ldk::chain::Filter;
use lampo_common::ldk::routing::utxo::UtxoResult;
use lampo_common::model::response::{NewAddress, Utxo};
use lampo_common::wallet::WalletManager;
use lampo_common::{async_trait, error};

pub struct LampoArkWallet {
    pub inner: Arc<BDKWalletManager>,
    pub backend: LampoChainSync,

    pub outputs_queue: Mutex<HashSet<WatchedOutput>>,
    pub txids: Mutex<HashMap<Txid, ScriptBuf>>,
}

#[async_trait]
impl WalletManager for LampoArkWallet {
    async fn new(conf: Arc<LampoConf>) -> error::Result<(Self, String)> {
        let (wallet, mnemonic_words) = BDKWalletManager::new(conf.clone()).await?;
        let backend = LampoChainSync::new(conf.clone())?;
        Ok((
            Self {
                inner: Arc::new(wallet),
                backend,

                outputs_queue: Mutex::new(HashSet::new()),
                txids: Mutex::new(HashMap::new()),
            },
            mnemonic_words,
        ))
    }

    async fn restore(conf: Arc<LampoConf>, mnemonic_words: &str) -> error::Result<Self> {
        let wallet = BDKWalletManager::restore(conf.clone(), mnemonic_words).await?;
        let backend = LampoChainSync::new(conf.clone())?;
        Ok(Self {
            inner: Arc::new(wallet),
            backend,

            outputs_queue: Mutex::new(HashSet::new()),
            txids: Mutex::new(HashMap::new()),
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
        self.inner.clone().listen().await
    }
}

#[async_trait]
impl Backend for LampoArkWallet {
    /// Return the kind of backend
    fn kind(&self) -> BackendKind {
        BackendKind::Core
    }

    /// Fetch feerate give a number of blocks
    ///
    /// FIXME: use `FeeRate` instead of `u32`
    async fn fee_rate_estimation(&self, blocks: u64) -> error::Result<u32> {
        todo!()
    }

    async fn minimum_mempool_fee(&self) -> error::Result<u32> {
        todo!()
    }

    async fn brodcast_tx(&self, tx: &Transaction) {
        todo!()
    }

    async fn get_utxo(&self, block: &BlockHash, idx: u64) -> UtxoResult {
        todo!()
    }

    async fn get_utxo_by_txid(&self, txid: &Txid, script: &Script) -> error::Result<TxResult> {
        todo!()
    }

    /// Get the information of a transaction inside the blockchain.
    async fn get_transaction(&self, txid: &Txid) -> error::Result<TxResult> {
        todo!()
    }

    /// Spawn a thread and start polling the backend and notify
    /// the listener through the handler.
    async fn listen(self: Arc<Self>) -> error::Result<()> {
        todo!()
    }
}

// FIXME: If we use the Filter we can drop the BlockSource?
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

impl Filter for LampoArkWallet {
    fn register_output(&self, output: lampo_common::backend::WatchedOutput) {
        self.outputs_queue.lock().unwrap().insert(output);
    }

    fn register_tx(&self, txid: &lampo_common::bitcoin::Txid, script_pubkey: &Script) {
        self.txids
            .lock()
            .unwrap()
            .insert(*txid, script_pubkey.into());
    }
}
