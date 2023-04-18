//! Lampo deamon implementation.
#![feature(async_fn_in_trait)]
pub mod actions;
pub mod chain;
pub mod keys;
pub mod ln;
pub mod persistence;
pub mod utils;

use std::sync::Arc;

use bitcoin::locktime::Height;
use lightning::{routing::gossip::P2PGossipSync, util::events::Event};
use lightning_background_processor::BackgroundProcessor;

use lampo_common::backend::Backend;
use lampo_common::conf::LampoConf;
use lampo_common::error;

use actions::handler::LampoHandler;
use chain::LampoChainManager;
use keys::keys::LampoKeys;
use ln::{peer_manager::LampoPeerManager, LampoChannelManager};
use persistence::LampoPersistence;
use utils::logger::LampoLogger;

use crate::actions::Handler;

pub struct LampoDeamon {
    conf: LampoConf,
    peer_manager: Option<Arc<LampoPeerManager>>,
    onchain_manager: Option<Arc<LampoChainManager>>,
    channel_manager: Option<Arc<LampoChannelManager>>,
    logger: Arc<LampoLogger>,
    persister: Arc<LampoPersistence>,
    handler: Option<Arc<LampoHandler>>,
}

impl<'ctx: 'static> LampoDeamon {
    pub fn new(config: LampoConf) -> Self {
        let root_path = config.path();
        LampoDeamon {
            conf: config,
            logger: Arc::new(LampoLogger {}),
            persister: Arc::new(LampoPersistence::new(root_path)),
            peer_manager: None,
            onchain_manager: None,
            channel_manager: None,
            handler: None,
        }
    }

    pub fn init_onchaind(
        &mut self,
        client: Arc<dyn Backend>,
        keys: Arc<LampoKeys>,
    ) -> error::Result<()> {
        let onchain_manager = LampoChainManager::new(client, keys);
        self.onchain_manager = Some(Arc::new(onchain_manager));
        Ok(())
    }

    pub fn onchain_manager(&self) -> Arc<LampoChainManager> {
        let manager = self.onchain_manager.clone().unwrap();
        manager.clone()
    }

    pub async fn init_channeld(&mut self) -> error::Result<()> {
        let mut manager = LampoChannelManager::new(
            &self.conf,
            self.logger.clone(),
            self.onchain_manager(),
            self.persister.clone(),
        );
        let (block_hash, Some(height)) = self
            .onchain_manager()
            .backend
            .get_best_block()
            .await.unwrap() else { error::bail!("wrong result with from the `get_best_block` call") };
        if let Err(err) = manager
            .start(block_hash, Height::from_consensus(height).unwrap())
            .await
        {
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
        peer_manager.init(&self.onchain_manager(), &self.channel_manager())?;
        self.peer_manager = Some(Arc::new(peer_manager));
        Ok(())
    }

    pub fn peer_manager(&self) -> Arc<LampoPeerManager> {
        let manager = self.peer_manager.clone().unwrap();
        manager.clone()
    }

    pub fn init_event_handler(&mut self) -> error::Result<()> {
        Ok(())
    }

    pub fn handler(&self) -> Arc<LampoHandler> {
        self.handler.clone().unwrap()
    }

    pub async fn init(
        &mut self,
        client: Arc<dyn Backend>,
        keys: Arc<LampoKeys>,
    ) -> error::Result<()> {
        self.init_onchaind(client.clone(), keys.clone())?;
        self.init_channeld().await?;
        self.init_peer_manager()?;
        Ok(())
    }

    pub async fn listen(self) -> error::Result<()> {
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
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::{net::SocketAddr, str::FromStr};

    use clightningrpc_conf::CLNConf;
    use lightning::util::config::UserConfig;

    use lampo_common::conf::LampoConf;
    use lampo_common::logger;
    use lampo_nakamoto::{Config, Network};

    use crate::{
        keys::keys::LampoKeys,
        ln::events::{NodeId, PeerEvents},
        LampoDeamon,
    };

    #[tokio::test]
    async fn simple_node_connection() {
        logger::init(log::Level::Debug).expect("initializing logger for the first time");
        let conf = LampoConf {
            ldk_conf: UserConfig::default(),
            network: bitcoin::Network::Testnet,
            port: 19753,
            path: "/tmp".to_string(),
            inner: CLNConf::new("/tmp/".to_owned(), true),
        };
        let mut lampo = LampoDeamon::new(conf);

        let mut conf = Config::default();
        conf.network = Network::Testnet;
        let client = Arc::new(lampo_nakamoto::Nakamoto::new(conf).unwrap());

        let result = lampo.init(client, Arc::new(LampoKeys::new())).await;
        assert!(result.is_ok());

        let node_id =
            NodeId::from_str("02049b60c296ffead3e7c8b124c5730153403a8314c1116c2d1b43cf9ac0de2d9d")
                .unwrap();
        let addr = SocketAddr::from_str("78.46.220.4:19735").unwrap();
        let result = lampo.peer_manager().connect(node_id, addr).await;
        assert!(result.is_ok(), "{:?}", result);
    }
}
