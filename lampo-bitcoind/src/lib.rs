//! Implementation of the bitcoin backend for
//! lampo.
use bitcoincore_rpc::Client;
use bitcoincore_rpc::Result;
use bitcoincore_rpc::RpcApi;

use bitcoincore_rpc::bitcoin::hashes::Hash;
use lampo_common::backend::Backend;
use lampo_common::backend::BlockHash;
use lampo_common::backend::BlockSourceResult;
use lampo_common::backend::{deserialize, serialize};
use lampo_common::backend::{Block, BlockData};

pub struct BitcoinCore {
    inner: Client,
}

impl BitcoinCore {
    pub fn new(url: &str, user: &str, pass: &str) -> Result<Self> {
        // FIXME: the bitcoincore_rpc do not support the https protocol.
        use bitcoincore_rpc::Auth;
        Ok(Self {
            inner: Client::new(url, Auth::UserPass(user.to_owned(), pass.to_owned()))?,
        })
    }
}

macro_rules! sync {
    ($expr: expr) => {
        Box::pin(async move { $expr })
    };
}

impl Backend for BitcoinCore {
    fn brodcast_tx(&self, tx: &lampo_common::backend::Transaction) {
        // FIXME: check the result.
        let _ = self.inner.send_raw_transaction(&serialize(tx));
    }

    fn fee_rate_estimation(&self, blocks: u64) -> u32 {
        // FIXME: manage the error here.
        let Ok(result) = self.inner.estimate_smart_fee(blocks as u16, None) else {
            return 0;
        };
        // FIXME: check what is the value that ldk want
        result.fee_rate.unwrap_or_default().to_btc() as u32
    }

    fn get_best_block<'a>(
        &'a self,
    ) -> lampo_common::backend::AsyncBlockSourceResult<(
        lampo_common::backend::BlockHash,
        Option<u32>,
    )> {
        let block = self.inner.get_chain_tips().unwrap().clone();
        let block = block.last().unwrap().clone();

        // FIXME: fix the rust bitcoin dependencies
        let hash: BlockHash = deserialize(&serialize(&block.hash.to_byte_array())).unwrap();
        sync! {
            BlockSourceResult::Ok((hash, Some(block.height as u32)))
        }
    }

    fn get_block<'a>(
        &'a self,
        header_hash: &'a lampo_common::backend::BlockHash,
    ) -> lampo_common::backend::AsyncBlockSourceResult<'a, lampo_common::backend::BlockData> {
        use bitcoincore_rpc::bitcoin::BlockHash;
        // FIXME: add in bitcoin core the from method
        use bitcoincore_rpc::bitcoin::consensus::serialize as inner_serialize;

        // FIXME: change the version of rust bitcoin in nakamoto and in lampod_common.
        let bytes = serialize(header_hash);
        let hash = BlockHash::from_slice(bytes.as_slice()).unwrap();
        let result = self.inner.get_block(&hash).unwrap();
        let block: Block = deserialize(&inner_serialize(&result)).unwrap();

        sync! {
           Ok(BlockData::FullBlock(block))
        }
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
}
