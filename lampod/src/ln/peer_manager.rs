use std::net::SocketAddr;
use std::time::Duration;
use std::{sync::Arc, time::SystemTime};

use lightning::ln::peer_handler::MessageHandler;
use lightning::ln::peer_handler::{IgnoringMessageHandler, PeerManager, SimpleArcPeerManager};
use lightning::onion_message::OnionMessenger;
use lightning::routing::gossip::P2PGossipSync;
use lightning_net_tokio;
use lightning_net_tokio::SocketDescriptor;

use lampo_common::conf::LampoConf;
use lampo_common::error;

use crate::chain::LampoChainManager;
use crate::utils::logger::LampoLogger;

use super::events::PeerEvents;
use super::{LampoChainMonitor, LampoChannelManager};

type InnerLampoPeerManager = SimpleArcPeerManager<
    SocketDescriptor,
    LampoChainMonitor,
    LampoChainManager,
    LampoChainManager,
    LampoChainManager,
    LampoLogger,
>;

pub struct LampoPeerManager {
    peer_manager: Option<Arc<InnerLampoPeerManager>>,
    conf: LampoConf,
    logger: Arc<LampoLogger>,
}

impl LampoPeerManager {
    pub fn new(conf: &LampoConf, logger: Arc<LampoLogger>) -> LampoPeerManager {
        LampoPeerManager {
            peer_manager: None,
            conf: conf.to_owned(),
            logger,
        }
    }

    pub fn manager(&self) -> Arc<InnerLampoPeerManager> {
        let manager = self.peer_manager.clone().unwrap();
        manager
    }

    pub fn init(
        &mut self,
        onchain_manager: &Arc<LampoChainManager>,
        channel_manager: &Arc<LampoChannelManager>,
    ) -> error::Result<()> {
        let ephemeral_bytes = [0; 32];
        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let onion_messenger = Arc::new(OnionMessenger::new(
            onchain_manager.keymanager.inner(),
            onchain_manager.keymanager.inner(),
            self.logger.clone(),
            IgnoringMessageHandler {},
        ));

        let gossip_sync = Arc::new(P2PGossipSync::new(
            channel_manager.graph(),
            Some(onchain_manager.clone()),
            self.logger.clone(),
        ));

        let lightning_msg_handler = MessageHandler {
            chan_handler: channel_manager.channeld.clone().unwrap(),
            onion_message_handler: onion_messenger,
            route_handler: gossip_sync.clone(),
        };
        let ignoring_custom_msg_handler = IgnoringMessageHandler {};

        let peer_manager = PeerManager::new(
            lightning_msg_handler,
            current_time.try_into().unwrap(),
            &ephemeral_bytes,
            channel_manager.logger.clone(),
            ignoring_custom_msg_handler,
            onchain_manager.keymanager.inner(),
        );
        self.peer_manager = Some(Arc::new(peer_manager));
        Ok(())
    }

    pub async fn run(self) -> error::Result<()> {
        let listen_port = self.conf.port;
        let Some(peer_manager) = self.peer_manager else {
            error::bail!("peer manager is None, at this point this should be not None");
        };
        let peer_manager = peer_manager.clone();
        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", listen_port)).await?;
        loop {
            let peer_manager = peer_manager.clone();
            let tcp_stream = listener.accept().await?.0;
            tokio::spawn(async move {
                // Use LDK's supplied networking battery to facilitate inbound
                // connections.
                lightning_net_tokio::setup_inbound(
                    peer_manager.clone(),
                    tcp_stream.into_std().unwrap(),
                )
                .await;
            });
        }
    }
}

impl PeerEvents for LampoPeerManager {
    async fn connect(&self, node_id: super::events::NodeId, host: SocketAddr) -> error::Result<()> {
        let Some(close_callback) = lightning_net_tokio::connect_outbound(self.manager(), node_id, host).await else {
          error::bail!("impossible connect with the peer `{node_id}`");
        };
        let mut connection_closed_future = Box::pin(close_callback);
        let manager = self.manager();
        loop {
            match futures::poll!(&mut connection_closed_future) {
                std::task::Poll::Ready(_) => {
                    log::info!("node `{node_id}` disconnected");
                    return Ok(());
                }
                std::task::Poll::Pending => {}
            }
            // Avoid blocking the tokio context by sleeping a bit
            match manager
                .get_peer_node_ids()
                .iter()
                .find(|(id, _)| *id == node_id)
            {
                Some(_) => return Ok(()),
                None => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    }

    async fn disconnect(&self, node_id: super::events::NodeId) -> error::Result<()> {
        //check for open channels with peer

        //check the pubkey matches a valid connected peer
        let peers = self.manager().get_peer_node_ids();
        if !peers.iter().any(|(pk, _)| &node_id == pk) {
            error::bail!("Error: Could not find peer `{node_id}`");
        }

        self.manager().disconnect_by_node_id(node_id);
        Ok(())
    }
}
