//! Implementation of the bitcoin backend for
//! lampo.
use std::cell::RefCell;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use bitcoincore_rpc::bitcoin::hashes::Hash;
use bitcoincore_rpc::Client;
use bitcoincore_rpc::Result;
use bitcoincore_rpc::RpcApi;

use lampo_common::backend::BlockHash;
use lampo_common::backend::{deserialize, serialize};
use lampo_common::backend::{Backend, TxResult};
use lampo_common::backend::{Block, BlockData};
use lampo_common::bitcoin::locktime::Height;
use lampo_common::bitcoin::{BlockHeader, Transaction, Txid};
use lampo_common::error;
use lampo_common::event::onchain::OnChainEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json;
use lampo_common::secp256k1::hashes::hex::ToHex;

pub struct BitcoinCore {
    inner: Client,
    handler: RefCell<Option<Arc<dyn Handler>>>,
    txs: RefCell<Mutex<Vec<Txid>>>,
    // receive notification if the
    // deamon was stop
    stop: Arc<bool>,
    pool_time: Duration,
    best_height: RefCell<u64>,
    last_bloch_hash: RefCell<Option<BlockHash>>,
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
    ) -> Result<Self> {
        // FIXME: the bitcoincore_rpc do not support the https protocol.
        use bitcoincore_rpc::Auth;
        Ok(Self {
            inner: Client::new(url, Auth::UserPass(user.to_owned(), pass.to_owned()))?,
            handler: RefCell::new(None),
            txs: RefCell::new(Mutex::new(Vec::new())),
            // by default the we pool bitcoind each 2 minutes
            pool_time: Duration::from_secs(pool_time.unwrap_or(120) as u64),
            stop,
            last_bloch_hash: None.into(),
            best_height: 0.into(),
        })
    }
}

impl Backend for BitcoinCore {
    fn brodcast_tx(&self, tx: &lampo_common::backend::Transaction) {
        // FIXME: check the result.
        let result: Result<json::Value> = self.inner.call(
            "sendrawtransaction",
            &[lampo_common::bitcoin::consensus::serialize(&tx)
                .to_hex()
                .into()],
        );
        log::info!(target: "bitcoind", "broadcast transaction return {:?}", result);
        if result.is_ok() {
            self.txs.borrow_mut().lock().unwrap().push(tx.txid());
            let handler = self.handler.borrow();
            let Some(handler) = handler.as_ref() else {
                return;
            };
            handler.emit(Event::OnChain(OnChainEvent::SendRawTransaction(tx.clone())));
        }
    }

    fn fee_rate_estimation(&self, blocks: u64) -> u32 {
        // FIXME: manage the error here.
        let Ok(result) = self.inner.estimate_smart_fee(blocks as u16, None) else {
            log::error!("failing to estimate fee");
            if self.inner.get_blockchain_info().unwrap().chain == "regtest" {
                return 100;
            }
            return 0;
        };
        // FIXME: check what is the value that ldk want
        let result = result.fee_rate.unwrap_or_default().to_sat() as u32;
        if result == 0 {
            return 1100;
        }
        result
    }

    fn get_best_block<'a>(
        &'a self,
    ) -> error::Result<(lampo_common::backend::BlockHash, Option<u32>)> {
        let block = self.inner.get_blockchain_info().unwrap().clone();
        // FIXME: fix the rust bitcoin dependencies
        let hash: BlockHash =
            deserialize(&serialize(&block.best_block_hash.to_byte_array())).unwrap();

        log::trace!(target: "bitcoind", "best block with hash `{hash}` at height {}", block.blocks);
        Ok((hash, Some(block.blocks as u32)))
    }

    fn get_block<'a>(
        &'a self,
        header_hash: &'a lampo_common::backend::BlockHash,
    ) -> error::Result<lampo_common::backend::BlockData> {
        use bitcoincore_rpc::bitcoin::BlockHash;
        // FIXME: add in bitcoin core the from method
        use bitcoincore_rpc::bitcoin::consensus::serialize as inner_serialize;

        // FIXME: change the version of rust bitcoin in nakamoto and in lampod_common.
        let bytes = serialize(header_hash);
        let hash = BlockHash::from_slice(bytes.as_slice()).unwrap();
        let result = self.inner.get_block(&hash).unwrap();
        let block: Block = deserialize(&inner_serialize(&result)).unwrap();
        let last_block = self.last_bloch_hash.borrow();

        let new = if let Some(last_hash) = last_block.as_ref() {
            hash.to_string() == last_hash.to_string()
        } else {
            false
        };

        if new {
            let _ = self.handler.borrow().clone().and_then(|handler| {
                handler.emit(Event::OnChain(OnChainEvent::NewBlock(block.clone())));
                Some(handler)
            });
        }
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
        _output: lampo_common::backend::WatchedOutput,
    ) -> Option<(usize, lampo_common::backend::Transaction)> {
        unimplemented!()
    }

    fn watch_utxo(
        &self,
        _txid: &lampo_common::backend::Txid,
        _script: &lampo_common::backend::Script,
    ) {
        unimplemented!()
    }

    fn get_transaction(&self, txid: &lampo_common::bitcoin::Txid) -> error::Result<TxResult> {
        let tx = self.inner.get_transaction(
            &bitcoincore_rpc::bitcoin::Txid::from_str(txid.to_string().as_str())?,
            None,
        )?;
        if let Some(true) = tx.details.first().unwrap().abandoned {
            return Ok(TxResult::Discarded);
        }
        let raw_tx: Transaction = deserialize(&tx.hex)?;
        if tx.info.confirmations > 0 {
            let block_hash = tx.info.blockhash.unwrap().to_string();
            let BlockData::FullBlock(block) = self.get_block(&BlockHash::from_str(&block_hash)?)?
            else {
                unreachable!()
            };
            let height = tx.info.blockheight.unwrap();
            // SAFETY: the first element should be always present?
            let idx = tx.details.first().unwrap().vout;
            return Ok(TxResult::Confirmed((
                raw_tx,
                idx,
                block.header,
                Height::from_consensus(height)?,
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
        let txs = self.txs.borrow_mut();
        let mut txs = txs.lock().unwrap();
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
        txs.append(&mut confirmed_txs);
        txs.append(&mut unconfirmed_txs);
        Ok(())
    }

    fn manage_transactions(&self, txs: &mut Vec<Txid>) -> lampo_common::error::Result<()> {
        let transactions = self.txs.borrow_mut();
        let mut transactions = transactions.lock().unwrap();
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
        log::info!(target: "bitcoin", "preparing bitcoind polling ...");
        Ok(std::thread::spawn(move || {
            log::info!(target: "bitcoind", "poolling bitcoind for on chain events");
            while !self.stop.as_ref() {
                let Ok((block_hash, height)) = self.get_best_block() else {
                    log::warn!(target: "bitcoind", "error while querying the best block");
                    continue;
                };
                let Ok(lampo_common::backend::BlockData::FullBlock(block)) =
                    self.get_block(&block_hash)
                else {
                    log::warn!(target: "bitcoind", "impossible retrieval the block with hash `{block_hash}`");
                    continue;
                };
                if height.unwrap_or_default() as u64 > self.best_height.borrow().clone() {
                    *self.best_height.borrow_mut() = height.unwrap().clone().into();
                    handler.emit(Event::OnChain(OnChainEvent::NewBestBlock((
                        block.header,
                        Height::from_consensus(height.unwrap_or_default()).unwrap(),
                    ))));
                }
                let _ = self.process_transactions();
                // Emit new Best block!
                std::thread::sleep(self.pool_time);
            }
        }))
    }
}
