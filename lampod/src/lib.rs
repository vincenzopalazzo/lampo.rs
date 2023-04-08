//! Lampo deamon implementation.
pub mod backend;
pub mod chain;
pub mod conf;
pub mod keys;
pub mod ln;
pub mod persistence;
pub mod utils;

use std::sync::Arc;

use lightning::{routing::gossip::P2PGossipSync, util::events::Event};
use lightning_background_processor::BackgroundProcessor;

use backend::Backend;
use chain::LampoChainManager;
use conf::LampoConf;
use keys::keys::LampoKeys;
use ln::{peer_manager::LampoPeerManager, LampoChannelManager};
use persistence::LampoPersistence;
use utils::logger::LampoLogger;

pub struct LampoDeamon {
    conf: LampoConf,
    peer_manager: Option<Arc<LampoPeerManager>>,
    onchain_manager: Option<Arc<LampoChainManager>>,
    channel_manager: Option<Arc<LampoChannelManager>>,
    logger: Arc<LampoLogger>,
    persister: Arc<LampoPersistence>,
}

impl LampoDeamon {
    pub fn new(config: LampoConf) -> Self {
        let root_path = config.path.clone();
        LampoDeamon {
            conf: config,
            logger: Arc::new(LampoLogger {}),
            persister: Arc::new(LampoPersistence::new(root_path)),
            peer_manager: None,
            onchain_manager: None,
            channel_manager: None,
        }
    }

    pub fn init_onchaind(
        &mut self,
        client: Arc<dyn Backend>,
        keys: Arc<LampoKeys>,
    ) -> Result<(), ()> {
        let onchain_manager = LampoChainManager::new(client, keys);
        self.onchain_manager = Some(Arc::new(onchain_manager));
        Ok(())
    }

    pub fn onchain_manager(&self) -> Arc<LampoChainManager> {
        let manager = self.onchain_manager.clone().unwrap();
        manager.clone()
    }

    pub fn init_channeld(&mut self) -> Result<(), ()> {
        let manager = LampoChannelManager::new(
            &self.conf,
            self.logger.clone(),
            self.onchain_manager(),
            self.persister.clone(),
        );
        self.channel_manager = Some(Arc::new(manager));
        Ok(())
    }

    pub fn channel_manager(&self) -> Arc<LampoChannelManager> {
        let manager = self.channel_manager.clone().unwrap();
        manager.clone()
    }

    pub fn init_peer_manager(&mut self) -> Result<(), ()> {
        let peer_manager = LampoPeerManager::new(&self.conf);
        peer_manager.init(&self.onchain_manager(), &self.channel_manager())?;
        self.peer_manager = Some(Arc::new(peer_manager));
        Ok(())
    }

    pub fn peer_manager(&self) -> Arc<LampoPeerManager> {
        let manager = self.peer_manager.clone().unwrap();
        manager.clone()
    }

    pub fn init(&mut self, client: &Arc<dyn Backend>, keys: &Arc<LampoKeys>) -> Result<(), ()> {
        self.init_onchaind(client.clone(), keys.clone())?;
        self.init_channeld()?;
        self.init_peer_manager()?;
        Ok(())
    }

    pub async fn listen(&self) -> Result<(), ()> {
        let event_handler = move |_: Event| {};

        let gossip_sync = Arc::new(P2PGossipSync::new(
            self.channel_manager().graph(),
            None::<Arc<LampoChainManager>>,
            self.logger.clone(),
        ));

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
}
