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

macro_rules! sync {
    ($expr: expr) => {
        Box::pin(async move { $expr })
    };
}

#[allow(unused_variables)]
impl Backend for Nakamoto {
    fn get_block<'a>(
        &'a self,
        header_hash: &'a nakamoto_common::block::BlockHash,
    ) -> AsyncBlockSourceResult<'a, BlockData> {
        let blk_chan = self.nakamoto.blocks();
        let Some(block) = self.nakamoto.get_block(header_hash).unwrap() else {
            unimplemented!();
        };
        log::info!("get block information {:?}", block);
        self.current_height.set(Some(block.0));

        let _ = self.handler.borrow().clone().and_then(|handler| {
            let (blk, _) = blk_chan.recv().unwrap();
            handler.emit(Event::OnChain(OnChainEvent::NewBlock(blk)));
            Some(handler)
        });
        sync! { Ok(BlockData::HeaderOnly(block.1)) }
    }

    fn watch_utxo(&self, _: &nakamoto_common::bitcoin::Txid, _: &nakamoto_common::bitcoin::Script) {
        todo!()
    }

    fn get_header<'a>(
        &'a self,
        _: &'a nakamoto_common::block::BlockHash,
        _: Option<u32>,
    ) -> AsyncBlockSourceResult<'a, BlockHeaderData> {
        todo!()
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

    fn get_best_block<'a>(
        &'a self,
    ) -> AsyncBlockSourceResult<(nakamoto_common::block::BlockHash, Option<u32>)> {
        let tip = self.nakamoto.get_tip().unwrap();
        sync! { Ok((tip.blk_header.block_hash(), Some(tip.height as u32))) }
    }

    fn register_output(
        &self,
        _: WatchedOutput,
    ) -> Option<(usize, nakamoto_common::block::Transaction)> {
        todo!()
    }

    fn fee_rate_estimation(&self, blocks: u64) -> u32 {
        let fee_rates: HashMap<String, f64> = self.rest.get_fee_estimates().unwrap();
        Nakamoto::fee_in_range(&fee_rates, 1, blocks + 2).unwrap() as u32
    }

    fn get_utxo(&self, block: &BlockHash, idx: u64) -> UtxoResult {
        todo!()
    }

    fn set_handler(&self, handler: std::sync::Arc<dyn lampo_common::handler::Handler>) {
        self.handler.replace(Some(handler));
    }
}
