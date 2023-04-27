//! Lampo deamon implementation.
pub mod actions;
mod builtin;
pub mod chain;
pub mod events;
pub mod handler;
pub mod jsonrpc;
pub mod keys;
pub mod ln;
pub mod persistence;
pub mod utils;

use std::{cell::Cell, sync::Arc};

use bitcoin::locktime::Height;
use chain::WalletManager;
use crossbeam_channel as chan;
use events::LampoEvent;
use futures::lock::Mutex;
use handler::external_handler::ExternalHandler;
use lightning::{events::Event, routing::gossip::P2PGossipSync};
use lightning_background_processor::BackgroundProcessor;

use lampo_common::backend::Backend;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::json;
use lampo_jsonrpc::json_rpc2::Request;

use actions::handler::LampoHandler;
use chain::LampoChainManager;
use ln::{LampoChannelManager, LampoInventoryManager, LampoPeerManager};
use persistence::LampoPersistence;
use tokio::runtime::Runtime;
use utils::logger::LampoLogger;

use crate::actions::Handler;

pub struct LampoDeamon {
    conf: LampoConf,
    peer_manager: Option<Arc<LampoPeerManager>>,
    onchain_manager: Option<Arc<LampoChainManager>>,
    channel_manager: Option<Arc<LampoChannelManager>>,
    inventory_manager: Option<Arc<LampoInventoryManager>>,
    wallet_manager: Arc<dyn WalletManager>,
    logger: Arc<LampoLogger>,
    persister: Arc<LampoPersistence>,
    handler: Option<Arc<LampoHandler>>,
    process: Arc<Mutex<Cell<Option<BackgroundProcessor>>>>,
    rt: Runtime,
}

unsafe impl Send for LampoDeamon {}
unsafe impl Sync for LampoDeamon {}

impl LampoDeamon {
    pub fn new(config: LampoConf, wallet_manager: Arc<dyn WalletManager>) -> Self {
        let root_path = config.path();
        LampoDeamon {
            conf: config,
            logger: Arc::new(LampoLogger {}),
            persister: Arc::new(LampoPersistence::new(root_path)),
            peer_manager: None,
            onchain_manager: None,
            channel_manager: None,
            inventory_manager: None,
            wallet_manager,
            handler: None,
            process: Arc::new(Mutex::new(Cell::new(None))),
            rt: Runtime::new().unwrap(),
        }
    }

    pub fn root_path(&self) -> String {
        self.conf.path()
    }

    pub fn conf(&self) -> &LampoConf {
        &self.conf
    }

    pub fn init_onchaind(&mut self, client: Arc<dyn Backend>) -> error::Result<()> {
        let onchain_manager = LampoChainManager::new(client, self.wallet_manager.clone());
        self.onchain_manager = Some(Arc::new(onchain_manager));
        Ok(())
    }

    pub fn onchain_manager(&self) -> Arc<LampoChainManager> {
        let manager = self.onchain_manager.clone().unwrap();
        manager.clone()
    }

    pub fn init_channeld(&mut self) -> error::Result<()> {
        let mut manager = LampoChannelManager::new(
            &self.conf,
            self.logger.clone(),
            self.onchain_manager().clone(),
            self.wallet_manager.clone(),
            self.persister.clone(),
        );
        let (block_hash, Some(height)) = async_run!(self.rt, self
            .onchain_manager()
            .backend
                                                    .get_best_block()).unwrap() else { error::bail!("wrong result with from the `get_best_block` call") };
        if let Err(err) = manager.start(block_hash, Height::from_consensus(height)?) {
            error::bail!("{err}");
        }
        self.channel_manager = Some(Arc::new(manager));
        Ok(())
    }

    pub fn channel_manager(&self) -> Arc<LampoChannelManager> {
        let manager = self.channel_manager.clone().unwrap();
        manager.clone()
    }

    pub fn init_peer_manager(&mut self) -> error::Result<()> {
        let mut peer_manager = LampoPeerManager::new(&self.conf, self.logger.clone());
        peer_manager.init(
            self.onchain_manager(),
            self.wallet_manager.clone(),
            self.channel_manager(),
        )?;
        self.peer_manager = Some(Arc::new(peer_manager));
        Ok(())
    }

    pub fn peer_manager(&self) -> Arc<LampoPeerManager> {
        let manager = self.peer_manager.clone().unwrap();
        manager.clone()
    }

    fn init_inventory_manager(&mut self) -> error::Result<()> {
        let manager = LampoInventoryManager::new(self.peer_manager(), self.channel_manager());
        self.inventory_manager = Some(Arc::new(manager));
        Ok(())
    }

    pub fn inventory_manager(&self) -> Arc<LampoInventoryManager> {
        let Some(ref manager) = self.inventory_manager else {
            panic!("inventory menager need to be initialized");
        };
        manager.clone()
    }

    pub fn wallet_manager(&self) -> Arc<dyn WalletManager> {
        self.wallet_manager.clone()
    }

    pub fn init_event_handler(&mut self) -> error::Result<()> {
        let handler = LampoHandler::new(&self);
        self.handler = Some(Arc::new(handler));
        Ok(())
    }

    pub fn handler(&self) -> Arc<LampoHandler> {
        self.handler.clone().unwrap()
    }

    pub fn init_reactor(&mut self) -> error::Result<()> {
        Ok(())
    }

    pub fn init(&mut self, client: Arc<dyn Backend>) -> error::Result<()> {
        self.init_onchaind(client.clone())?;
        self.init_channeld()?;
        self.init_peer_manager()?;
        self.init_inventory_manager()?;
        self.init_event_handler()?;
        Ok(())
    }

    pub fn add_external_handler(&self, ext_handler: Arc<dyn ExternalHandler>) -> error::Result<()> {
        let Some(ref handler) = self.handler else {
            error::bail!("Initial handler is None");
        };
        handler.add_external_handler(ext_handler)?;
        Ok(())
    }

    pub fn listen(&self) -> error::Result<()> {
        let gossip_sync = Arc::new(P2PGossipSync::new(
            self.channel_manager().graph(),
            None::<Arc<LampoChainManager>>,
            self.logger.clone(),
        ));

        let handler = self.handler();
        let event_handler = move |event: Event| {
            log::info!("ldk event {:?}", event);
            if let Err(err) = handler.handle(event) {
                log::error!("{err}");
            }
        };

        let background_processor = BackgroundProcessor::start(
            self.persister.clone(),
            event_handler,
            self.channel_manager().chain_monitor(),
            self.channel_manager().manager(),
            lightning_background_processor::GossipSync::p2p(gossip_sync),
            self.peer_manager().manager(),
            self.logger.clone(),
            Some(self.channel_manager().scorer()),
        );
        let _ = background_processor.join();
        Ok(())
    }

    pub fn call(&self, method: &str, args: json::Value) -> error::Result<json::Value> {
        let request = Request::new(method, args);
        let (sender, receiver) = chan::bounded::<json::Value>(1);
        let command = LampoEvent::from_req(&request, &sender)?;
        log::info!("received {:?}", command);
        let Some(ref handler) = self.handler else {
            error::bail!("at this point the handler should be not None");
        };
        handler.react(command)?;
        Ok(receiver.recv()?)
    }

    pub async fn stop(self) -> error::Result<()> {
        if let Some(process) = self.process.lock().await.take() {
            process.stop()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use clightningrpc_conf::CLNConf;
    use lightning::util::config::UserConfig;

    use lampo_common::conf::LampoConf;
    use lampo_common::json;
    use lampo_common::logger;
    use lampo_common::model::request;
    use lampo_nakamoto::{Config, Network};

    use crate::chain::WalletManager;

    use crate::{async_run, chain::LampoWalletManager, ln::events::PeerEvents, LampoDeamon};

    #[test]
    fn simple_node_connection() {
        logger::init(log::Level::Debug).expect("initializing logger for the first time");
        let conf = LampoConf {
            ldk_conf: UserConfig::default(),
            network: bitcoin::Network::Testnet,
            port: 19753,
            path: "/tmp".to_string(),
            inner: CLNConf::new("/tmp/".to_owned(), true),
        };
        let wallet = LampoWalletManager::new(conf.network).unwrap();
        let mut lampo = LampoDeamon::new(conf, Arc::new(wallet));

        let mut conf = Config::default();
        conf.network = Network::Testnet;
        let client = Arc::new(lampo_nakamoto::Nakamoto::new(conf).unwrap());

        let result = lampo.init(client);
        assert!(result.is_ok());

        let connect = request::Connect {
            node_id: "02049b60c296ffead3e7c8b124c5730153403a8314c1116c2d1b43cf9ac0de2d9d"
                .to_owned(),
            addr: "78.46.220.4".to_owned(),
            port: 19735,
        };
        let result = async_run!(
            lampo.rt,
            lampo
                .peer_manager()
                .connect(connect.node_id(), connect.addr())
        );
        assert!(result.is_ok(), "{:?}", result);
    }

    #[test]
    fn simple_get_info() {
        let conf = LampoConf {
            ldk_conf: UserConfig::default(),
            network: bitcoin::Network::Testnet,
            port: 19753,
            path: "/tmp".to_string(),
            inner: CLNConf::new("/tmp/".to_owned(), true),
        };
        let wallet = LampoWalletManager::new(conf.network).unwrap();
        let mut lampo = LampoDeamon::new(conf, Arc::new(wallet));

        let mut conf = Config::default();
        conf.network = Network::Testnet;
        let client = Arc::new(lampo_nakamoto::Nakamoto::new(conf).unwrap());

        let result = lampo.init(client);
        assert!(result.is_ok());
        let payload = json::json!({});
        let result = lampo.call("getinfo", payload);
        assert!(result.is_ok(), "{:?}", result);
    }
}
