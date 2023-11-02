//! Nakamoto backend implementation for Lampo
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::Arc;

use esplora_client::BlockingClient;
use esplora_client::Builder;
use nakamoto_client::traits::Handle;
use nakamoto_common::block::Height;
use nakamoto_net_poll::Reactor;
use nakamoto_net_poll::Waker;

pub use nakamoto_client::{Client, Config, Error, Network};

use lampo_common::backend::AsyncBlockSourceResult;
use lampo_common::backend::Backend;
use lampo_common::backend::BlockData;
use lampo_common::backend::BlockHash;
use lampo_common::backend::BlockHeaderData;
use lampo_common::backend::UtxoResult;
use lampo_common::backend::WatchedOutput;
use lampo_common::error;
use lampo_common::event::onchain::OnChainEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;

#[derive(Clone)]
pub struct Nakamoto {
    nakamoto: nakamoto_client::Handle<Waker>,
    current_height: Cell<Option<Height>>,
    rest: BlockingClient,
    handler: RefCell<Option<Arc<dyn Handler>>>,
}

impl Nakamoto {
    pub fn new(config: Config) -> error::Result<Self> {
        let nakamoto = Client::<Reactor<TcpStream>>::new()?;
        let handler = nakamoto.handle();
        let url = match config.network.as_str() {
            "bitcoin" => "https://blockstream.info/api",
            "testnet" => "https://blockstream.info/testnet/api",
            _ => {
                return Err(error::anyhow!(
                    "network {} not supported",
                    config.network.as_str()
                ))
            }
        };

        // FIXME: join this later
        let _worker = std::thread::spawn(|| nakamoto.run(config));
        let client = Nakamoto {
            nakamoto: handler,
            current_height: Cell::new(None),
            rest: Builder::new(url)
                .build_blocking()
                .map_err(|err| error::anyhow!("{err}"))?,
            handler: RefCell::new(None),
        };
        Ok(client)
    }

    fn fee_in_range(estimation: &HashMap<String, f64>, from: u64, to: u64) -> Option<i64> {
        for rate in from..to {
            let key = &format!("{rate}");
            if estimation.contains_key(key) {
                return Some(estimation[key] as i64);
            }
        }
        None
    }
}

#[allow(unused_variables)]
impl Backend for Nakamoto {
    fn get_block<'a>(
        &'a self,
        header_hash: &'a nakamoto_common::block::BlockHash,
    ) -> error::Result<BlockData> {
        let blk_chan = self.nakamoto.blocks();
        let block = self
            .nakamoto
            .get_block(header_hash)?
            .ok_or(error::anyhow!("block `{header_hash}` not found"))?;
        log::info!("get block information {:?}", block);
        self.current_height.set(Some(block.0));

        let _ = self.handler.borrow().clone().map(|handler| {
            let (blk, _) = blk_chan.recv().unwrap();
            handler.emit(Event::OnChain(OnChainEvent::NewBlock(blk)));
            handler
        });
        Ok(BlockData::HeaderOnly(block.1))
    }

    fn watch_utxo(&self, _: &nakamoto_common::bitcoin::Txid, _: &nakamoto_common::bitcoin::Script) {
        todo!()
    }

    fn get_header<'a>(
        &'a self,
        header_hash: &'a BlockHash,
        height_hint: Option<u32>,
    ) -> AsyncBlockSourceResult<'a, BlockHeaderData> {
        unimplemented!()
    }

    fn brodcast_tx(&self, tx: &nakamoto_common::block::Transaction) {
        let result = self.nakamoto.submit_transaction(tx.clone());
        if let Err(err) = result {
            log::error!("brodcast tx fails: {err}");
        } else {
            let handler = self.handler.borrow().clone().unwrap();
            handler.emit(Event::OnChain(OnChainEvent::SendRawTransaction(tx.clone())));
        }
    }

    fn is_lightway(&self) -> bool {
        true
    }

    fn get_best_block(&self) -> error::Result<(nakamoto_common::block::BlockHash, Option<u32>)> {
        let tip = self.nakamoto.get_tip()?;
        Ok((tip.blk_header.block_hash(), Some(tip.height as u32)))
    }

    fn register_output(
        &self,
        _: WatchedOutput,
    ) -> Option<(usize, nakamoto_common::block::Transaction)> {
        todo!()
    }

    fn fee_rate_estimation(&self, blocks: u64) -> error::Result<u32> {
        // There may be errors here?
        let fee_rates: HashMap<String, f64> = self.rest.get_fee_estimates().unwrap();
        return Ok(Nakamoto::fee_in_range(&fee_rates, 1, blocks + 2).unwrap() as u32);
    }

    fn minimum_mempool_fee(&self) -> error::Result<u32> {
        Ok(self.fee_rate_estimation(2).unwrap())
    }

    fn get_utxo(&self, block: &BlockHash, idx: u64) -> UtxoResult {
        todo!()
    }

    fn get_utxo_by_txid(
        &self,
        txid: &esplora_client::api::Txid,
        script: &esplora_client::Script,
    ) -> error::Result<lampo_common::backend::TxResult> {
        unimplemented!()
    }

    fn set_handler(&self, handler: std::sync::Arc<dyn lampo_common::handler::Handler>) {
        self.handler.replace(Some(handler));
    }

    fn get_transaction(
        &self,
        txid: &esplora_client::api::Txid,
    ) -> error::Result<lampo_common::backend::TxResult> {
        unimplemented!()
    }

    fn manage_transactions(&self, txs: &mut Vec<esplora_client::api::Txid>) -> error::Result<()> {
        unimplemented!()
    }

    fn listen(self: Arc<Self>) -> error::Result<std::thread::JoinHandle<()>> {
        unimplemented!()
    }

    fn process_transactions(&self) -> error::Result<()> {
        unimplemented!()
    }
}
