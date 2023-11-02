//! Implementation of the bitcoin backend for
//! lampo.
use std::cell::RefCell;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use bitcoincore_rpc::bitcoin::hashes::Hash;
use bitcoincore_rpc::bitcoincore_rpc_json::GetTxOutResult;
use bitcoincore_rpc::Client;
use bitcoincore_rpc::RpcApi;

use lampo_common::backend::BlockHash;
use lampo_common::backend::{deserialize, serialize};
use lampo_common::backend::{Backend, TxResult};
use lampo_common::backend::{Block, BlockData};
use lampo_common::bitcoin::locktime::Height;
use lampo_common::bitcoin::{Script, Transaction, Txid};
use lampo_common::error;
use lampo_common::event::onchain::OnChainEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json;
use lampo_common::secp256k1::hashes::hex::ToHex;

pub struct BitcoinCore {
    inner: Client,
    handler: RefCell<Option<Arc<dyn Handler>>>,
    ours_txs: Mutex<RefCell<Vec<Txid>>>,
    others_txs: Mutex<RefCell<Vec<(Txid, Script)>>>,
    // receive notification if the
    // deamon was stop
    stop: Arc<bool>,
    pool_time: Duration,
    best_height: RefCell<u64>,
    last_bloch_hash: RefCell<Option<BlockHash>>,
}

impl std::fmt::Debug for BitcoinCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "BitcoinCore {{ best_height: {:?}, last_bloch_hash: {:?} }}",
            self.best_height, self.last_bloch_hash
        )?;
        Ok(())
    }
}

// FIXME: fix this for bitcoin core struct
unsafe impl Send for BitcoinCore {}
unsafe impl Sync for BitcoinCore {}

impl BitcoinCore {
    pub fn new(
        url: &str,
        user: &str,
        pass: &str,
        stop: Arc<bool>,
        pool_time: Option<u8>,
    ) -> bitcoincore_rpc::Result<Self> {
        // FIXME: the bitcoincore_rpc do not support the https protocol.
        use bitcoincore_rpc::Auth;
        Ok(Self {
            inner: Client::new(url, Auth::UserPass(user.to_owned(), pass.to_owned()))?,
            handler: RefCell::new(None),
            ours_txs: Mutex::new(RefCell::new(Vec::new())),
            others_txs: Mutex::new(RefCell::new(Vec::new())),
            // by default the we pool bitcoind each 2 minutes
            pool_time: Duration::from_secs(pool_time.unwrap_or(120) as u64),
            stop,
            last_bloch_hash: None.into(),
            best_height: 0.into(),
        })
    }

    pub fn gettxout(&self, txid: &Txid, idx: u64) -> error::Result<Vec<u8>> {
        let tx: GetTxOutResult = self
            .inner
            .call("gettxout", &[txid.to_string().into(), idx.into()])?;
        Ok(tx.script_pub_key.hex)
    }

    pub fn watch_tx(&self, txid: &Txid, script: &Script) -> error::Result<()> {
        log::debug!(target: "bitcoind", "Looking an external transaction `{}`", txid);
        if self
            .ours_txs
            .lock()
            .unwrap()
            .borrow()
            .iter()
            .any(|&i| i.to_string() == txid.to_string())
        {
            return Ok(());
        }
        self.others_txs
            .lock()
            .unwrap()
            .borrow_mut()
            .push((txid.clone(), script.clone()));
        Ok(())
    }

    pub fn get_block_hash(&self, height: u64) -> error::Result<BlockHash> {
        let block_hash: BlockHash = self.inner.call("getblockhash", &[height.into()])?;
        Ok(block_hash)
    }

    pub fn find_tx_in_block(&self, block: &Block) -> error::Result<()> {
        log::debug!(target: "bitcoin", "looking the tx inside the new block");
        let utxos = self.others_txs.lock().unwrap();
        let mut utxos = utxos.borrow_mut();
        let mut still_unconfirmed: Vec<(Txid, Script)> = vec![];
        for (utxo, script) in utxos.iter() {
            log::debug!(target: "bitcoind", "looking for UTXO {} inside the block at height: {}", utxo, self.best_height.borrow());
            if let Some((idx, tx)) = block
                .txdata
                .iter()
                .enumerate()
                .find(|(_, tx)| tx.txid() == *utxo)
            {
                // Confirmed!
                let handler = self.handler.borrow();
                let handler = handler
                    .as_ref()
                    .ok_or(error::anyhow!("handler is not sent"))?;
                handler.emit(Event::OnChain(OnChainEvent::ConfirmedTransaction((
                    tx.clone(),
                    idx as u32,
                    block.header,
                    Height::from_consensus(self.best_height.borrow().clone() as u32)?,
                ))));
            } else {
                still_unconfirmed.push((utxo.clone(), script.clone()));
            }
        }
        utxos.clear();
        utxos.append(&mut still_unconfirmed);
        Ok(())
    }
}

impl Backend for BitcoinCore {
    fn brodcast_tx(&self, tx: &lampo_common::backend::Transaction) {
        // FIXME: check the result.
        let result: bitcoincore_rpc::Result<json::Value> = self.inner.call(
            "sendrawtransaction",
            &[lampo_common::bitcoin::consensus::serialize(&tx)
                .to_hex()
                .into()],
        );
        log::info!(target: "bitcoind", "broadcast transaction return {:?}", result);
        if result.is_ok() {
            self.ours_txs.lock().unwrap().borrow_mut().push(tx.txid());
            self.others_txs
                .lock()
                .unwrap()
                .borrow_mut()
                .retain(|(txid, _)| txid.to_string() == tx.txid().to_string());
            let handler = self.handler.borrow();
            let Some(handler) = handler.as_ref() else {
                return;
            };
            handler.emit(Event::OnChain(OnChainEvent::SendRawTransaction(tx.clone())));
        }
    }

    fn fee_rate_estimation(&self, blocks: u64) -> error::Result<u32> {
        let result = self
            .inner
            .estimate_smart_fee(blocks as u16, None)
            .map_err(|err| {
                log::error!("failing to estimate fee");
                let block_chain_info = self.inner.get_blockchain_info()?;
                if block_chain_info.chain == "regtest" {
                    return Ok(253);
                }
                Err(err)
            });

        if let Some(errors) = &result.as_ref().unwrap().errors {
            error::bail!(
                "{}",
                errors
                    .iter()
                    .map(|err| format!("{err}"))
                    .collect::<String>()
            );
        }
        let result: u32 = result.unwrap().fee_rate.unwrap_or_default().to_sat() as u32;
        if result == 0 {
            return Ok(253);
        }
        Ok(result)
    }

    fn minimum_mempool_fee(&self) -> error::Result<u32> {
        use lampo_common::btc_rpc::MinimumMempoolFee;

        let fee: MinimumMempoolFee = self.inner.call("getmempoolinfo", &[])?;
        // FIXME: adds the trait for conversion from and to BTC
        let fee = fee.mempoolminfee;
        Ok((fee * 10000 as f32) as u32)
    }

    fn get_best_block(&self) -> error::Result<(lampo_common::backend::BlockHash, Option<u32>)> {
        let block = self.inner.get_blockchain_info()?;
        // FIXME: fix the rust bitcoin dependencies
        let hash: BlockHash = deserialize(&serialize(&block.best_block_hash.to_byte_array()))?;

        log::trace!(target: "bitcoind", "best block with hash `{hash}` at height {}", block.blocks);
        Ok((hash, Some(block.blocks as u32)))
    }

    fn get_block(
        &self,
        header_hash: &lampo_common::backend::BlockHash,
    ) -> error::Result<lampo_common::backend::BlockData> {
        use bitcoincore_rpc::bitcoin::consensus::serialize as inner_serialize;
        use bitcoincore_rpc::bitcoin::BlockHash;

        // FIXME: change the version of rust bitcoin in nakamoto and in lampod_common.
        let bytes = serialize(header_hash);
        let hash = BlockHash::from_slice(bytes.as_slice())?;
        let result = self.inner.get_block(&hash)?;
        let block: Block = deserialize(&inner_serialize(&result))?;
        log::debug!(target: "bitcoind", "decode blocks {}", header_hash.to_string());
        Ok(BlockData::FullBlock(block))
    }

    fn get_header<'a>(
        &'a self,
        _header_hash: &'a lampo_common::backend::BlockHash,
        _height_hint: Option<u32>,
    ) -> lampo_common::backend::AsyncBlockSourceResult<'a, lampo_common::backend::BlockHeaderData>
    {
        unimplemented!("`get_header` is called only for lightway nodes");
    }

    fn get_utxo(
        &self,
        _block: &lampo_common::backend::BlockHash,
        _idx: u64,
    ) -> lampo_common::backend::UtxoResult {
        unimplemented!()
    }

    fn is_lightway(&self) -> bool {
        false
    }

    fn register_output(
        &self,
        output: lampo_common::backend::WatchedOutput,
    ) -> Option<(usize, lampo_common::backend::Transaction)> {
        self.watch_tx(&output.outpoint.txid, &output.script_pubkey)
            .unwrap();
        let _ = self.process_transactions();
        None
    }

    fn watch_utxo(
        &self,
        txid: &lampo_common::backend::Txid,
        script: &lampo_common::backend::Script,
    ) {
        self.watch_tx(txid, script).unwrap();
        let _ = self.process_transactions();
    }

    fn get_transaction(&self, txid: &lampo_common::bitcoin::Txid) -> error::Result<TxResult> {
        log::debug!(target: "bitcoind", "call get_transaction");
        let tx = self.inner.get_transaction(
            &bitcoincore_rpc::bitcoin::Txid::from_str(txid.to_string().as_str())?,
            None,
        )?;
        // SAFETY: the transaction should contains always the first.
        //
        // FIXME: we are looking at the first is always a good ide?
        if let Some(true) = tx.details.first().unwrap().abandoned {
            return Ok(TxResult::Discarded);
        }
        let raw_tx: Transaction = deserialize(&tx.hex)?;
        if tx.info.confirmations > 0 {
            // SAFETY: if it is confirmed, the block hash is not null.
            let block_hash = tx.info.blockhash.unwrap().to_hex();
            let BlockData::FullBlock(block) = self.get_block(&BlockHash::from_str(&block_hash)?)?
            else {
                unreachable!()
            };
            // SAFETY: if it is confirmed the block height should be not null.
            let height = tx.info.blockheight.unwrap();
            return Ok(TxResult::Confirmed((
                raw_tx,
                // SAFETY: this is safe to do because it is confirmed
                // and will be never null.
                tx.info.blockindex.unwrap() as u32,
                block.header,
                Height::from_consensus(height)?,
            )));
        }
        Ok(TxResult::Unconfirmed(raw_tx))
    }

    fn get_utxo_by_txid(
        &self,
        txid: &lampo_common::bitcoin::Txid,
        script: &lampo_common::bitcoin::Script,
    ) -> error::Result<TxResult> {
        let tx: bitcoincore_rpc::bitcoincore_rpc_json::GetRawTransactionResult = self
            .inner
            .call("getrawtransaction", &[txid.to_string().into(), true.into()])?;
        let raw_tx: Transaction = deserialize(&tx.hex)?;
        if tx.confirmations.is_some() {
            // SAFETY: if it is confirmed, the block hash is not null.
            let block_hash = tx.blockhash.unwrap().to_string();
            let BlockData::FullBlock(block) = self.get_block(&BlockHash::from_str(&block_hash)?)?
            else {
                unreachable!()
            };
            // SAFETY: the outpoint should be always present otherwise we are looking inside the wrong tx
            let outpoint = tx
                .vout
                .iter()
                .enumerate()
                .find(|vout| vout.1.script_pub_key.hex.to_hex() == script.to_hex())
                .unwrap();
            return Ok(TxResult::Confirmed((
                raw_tx,
                outpoint.0 as u32,
                block.header,
                // FIXME: this is correct?
                Height::from_consensus(block.bip34_block_height()? as u32)?,
            )));
        }
        Ok(TxResult::Unconfirmed(raw_tx))
    }

    fn set_handler(&self, handler: Arc<dyn Handler>) {
        self.handler.replace(Some(handler));
    }

    fn process_transactions(&self) -> lampo_common::error::Result<()> {
        let handler = self
            .handler
            .borrow()
            .clone()
            .ok_or(error::anyhow!("handler is not set"))?;
        let txs = self.ours_txs.lock().unwrap();
        let mut txs = txs.borrow_mut();
        let mut confirmed_txs: Vec<Txid> = Vec::new();
        let mut unconfirmed_txs: Vec<Txid> = Vec::new();
        for txid in txs.iter() {
            match self.get_transaction(txid)? {
                TxResult::Confirmed((tx, idx, header, height)) => {
                    confirmed_txs.push(tx.txid());
                    handler.emit(Event::OnChain(OnChainEvent::ConfirmedTransaction((
                        tx, idx, header, height,
                    ))))
                }
                TxResult::Unconfirmed(tx) => {
                    unconfirmed_txs.push(tx.txid());
                    handler.emit(Event::OnChain(OnChainEvent::UnconfirmedTransaction(
                        tx.txid(),
                    )));
                }
                TxResult::Discarded => {}
            }
        }
        txs.clear();
        // FIXME: if we want remember this we should put in a separate vector maybe?
        // or make it persistan.
        //
        // txs.append(&mut confirmed_txs);
        txs.append(&mut unconfirmed_txs);
        Ok(())
    }

    fn manage_transactions(&self, txs: &mut Vec<Txid>) -> lampo_common::error::Result<()> {
        let transactions = self.ours_txs.lock().unwrap();
        let mut transactions = transactions.borrow_mut();
        transactions.append(txs);
        self.process_transactions()
    }

    fn listen(self: Arc<Self>) -> error::Result<JoinHandle<()>> {
        let handler = self
            .handler
            .borrow()
            .clone()
            .ok_or(error::anyhow!("handler is not set"))?
            .clone();
        log::info!(target: "bitcoin", "Starting bitcoind polling ...");
        Ok(std::thread::spawn(move || {
            while !self.stop.as_ref() {
                log::trace!(target: "bitcoind", "Current Status during another iteration {:#?}", self);
                let best_block = self.get_best_block();
                let Ok((block_hash, height)) = best_block else {
                    // SAFETY: if we are in this block the error will be always not null
                    log::error!(target: "bitcoind", "Impossible get the inforamtion of the last besh block: {}", best_block.err().unwrap());
                    break;
                };
                if height.is_none() {
                    log::warn!(target: "bitcoind", "height is none for the best block found `{block_hash}`");
                    continue;
                }
                let height = height.unwrap();

                if !self.others_txs.lock().unwrap().borrow().is_empty() {
                    let start: u64 = self.best_height.borrow().clone().into();
                    let end: u64 = height.into();
                    log::trace!(target: "bitcoind", "Scan blocks in range [{start}..{end}]");
                    for height in start..end + 1 {
                        log::trace!(target: "bitcoind", "Looking at block with height {height}");
                        let block_hash = self.get_block_hash(height).unwrap();
                        let Ok(lampo_common::backend::BlockData::FullBlock(block)) =
                            self.get_block(&block_hash)
                        else {
                            log::warn!(target: "bitcoind", "Impossible retrieval the block information with hash `{block_hash}`");
                            continue;
                        };
                        if self.best_height.borrow().lt(&height.into()) {
                            *self.best_height.borrow_mut() = height.into();
                            *self.last_bloch_hash.borrow_mut() = Some(block_hash);
                            log::trace!(target: "bitcoind", "new best block with hash `{block_hash}` at height `{height}`");
                            handler.emit(Event::OnChain(OnChainEvent::NewBestBlock((
                                block.header,
                                // SAFETY: the height should be always a valid u32
                                Height::from_consensus(height as u32).unwrap(),
                            ))));

                            self.handler.borrow().clone().map(|handler| {
                                handler.emit(Event::OnChain(OnChainEvent::NewBlock(block.clone())));
                            });
                            let _ = self.find_tx_in_block(&block);
                        }
                    }
                    // ok when the wallet is full in sync with the blockchain, we can query the
                    // bitcoind wallet for ours transaction.
                    //
                    // This is the only place where we can query because otherwise the we can
                    // confuse ldk when we send a new best block with height X and a Confirmed transaction
                    // event at height Y, where Y > X. In this way ldk think that a reorgs happens.
                    //
                    // The reorgs do not happens, it is only that the bitcoind wallet is able to answer quickly
                    // while the lampo wallet is still looking for external transaction inside the blocks.
                    let _ = self.process_transactions();
                } else if self.best_height.borrow().lt(&height.into()) {
                    log::trace!(target: "bitcoind", "New best block at height {height}, out current best block is {}", self.best_height.borrow());
                    *self.best_height.borrow_mut() = height.into();
                    *self.last_bloch_hash.borrow_mut() = Some(block_hash);
                    let Ok(lampo_common::backend::BlockData::FullBlock(block)) =
                        self.get_block(&block_hash)
                    else {
                        log::warn!(target: "bitcoind", "Impossible retrieval the block information with hash `{block_hash}`");
                        continue;
                    };
                    handler.emit(Event::OnChain(OnChainEvent::NewBestBlock((
                        block.header,
                        // SAFETY: the height should be always a valid u32
                        Height::from_consensus(height).unwrap(),
                    ))));
                    let _ = self.find_tx_in_block(&block);
                    log::trace!(target: "bitcoind", "new best block with hash `{block_hash}` at height `{}`", height);
                }

                // Emit new Best block!
                std::thread::sleep(self.pool_time);
            }
        }))
    }
}
