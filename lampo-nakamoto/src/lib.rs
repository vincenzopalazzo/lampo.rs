//! Nakamoto backend implementation for Lampo
use std::cell::Cell;
use std::net::TcpStream;

use nakamoto_client::traits::Handle;
pub use nakamoto_client::{Client, Config, Error, Network};
use nakamoto_common::block::Height;
use nakamoto_net_poll::Reactor;
use nakamoto_net_poll::Waker;

use lampo_common::backend::AsyncBlockSourceResult;
use lampo_common::backend::Backend;
use lampo_common::backend::BlockData;
use lampo_common::backend::BlockHeaderData;
use lampo_common::backend::WatchedOutput;

#[derive(Clone)]
pub struct Nakamoto {
    handler: nakamoto_client::Handle<Waker>,
    current_height: Cell<Option<Height>>,
}

impl Nakamoto {
    pub fn new(config: Config) -> Result<Self, Error> {
        let nakamoto = Client::<Reactor<TcpStream>>::new()?;
        let handler = nakamoto.handle();
        // FIXME: join this later
        let _worker = std::thread::spawn(|| nakamoto.run(config));
        let client = Nakamoto {
            handler,
            current_height: Cell::new(None),
        };

        Ok(client)
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
        let Some(block) = self.handler.get_block(header_hash).unwrap() else {
            unimplemented!();
        };
        log::info!("get block information {:?}", block);
        self.current_height.set(Some(block.0));
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
        let result = self.handler.submit_transaction(tx.clone());
        if let Err(err) = result {
            log::error!("brodcast tx fails: {err}");
        };
    }

    fn is_lightway(&self) -> bool {
        true
    }

    fn get_best_block<'a>(
        &'a self,
    ) -> AsyncBlockSourceResult<(nakamoto_common::block::BlockHash, Option<u32>)> {
        let tip = self.handler.get_tip().unwrap();
        sync! { Ok((tip.blk_header.block_hash(), Some(tip.height as u32))) }
    }

    fn register_output(
        &self,
        _: WatchedOutput,
    ) -> Option<(usize, nakamoto_common::block::Transaction)> {
        todo!()
    }

    fn fee_rate_estimation(&self, blocks: u64) -> u32 {
        if self.current_height.get().is_none() {
            let Ok(block) = self.handler.get_tip() else {
              unreachable!()
          };
            self.current_height.set(Some(block.height));
        }
        let feerate = self
            .handler
            .estimate_feerate(self.current_height.get().unwrap() - blocks);
        if feerate.is_err() {
            log::error!("{:?}", feerate);
            panic!("{:?}", feerate);
        }

        log::info!("fee rate estimated {:?}", feerate);
        let Some(feerate) = feerate.unwrap() else {
           log::warn!("feerate not found for block {:?} - {blocks}", self.current_height);
            return 0;
        };
        feerate.median as u32
    }
}
