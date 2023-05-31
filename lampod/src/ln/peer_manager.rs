use std::net::SocketAddr;
use std::time::Duration;
use std::{sync::Arc, time::SystemTime};

use async_trait::async_trait;
use lightning::ln::peer_handler::MessageHandler;
use lightning::ln::peer_handler::{IgnoringMessageHandler, PeerManager};
use lightning::onion_message::OnionMessenger;
use lightning::routing::gossip::{NetworkGraph, P2PGossipSync};
use lightning_net_tokio;
use lightning_net_tokio::SocketDescriptor;

use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::keymanager::KeysManager;
use lampo_common::model::Connect;
use lampo_common::types::NodeId;

use crate::async_run;
use crate::chain::{LampoChainManager, WalletManager};
use crate::ln::LampoChannelManager;
use crate::utils::logger::LampoLogger;

use super::channe_manager::{LampoArcChannelManager, LampoChainMonitor};
use super::events::PeerEvents;
use super::peer_event;

pub type LampoArcOnionMessenger<L> =
    OnionMessenger<Arc<KeysManager>, Arc<KeysManager>, Arc<L>, IgnoringMessageHandler>;

pub type SimpleArcPeerManager<SD, M, T, F, C, L> = PeerManager<
    SD,
    Arc<LampoArcChannelManager<M, T, F, L>>,
    Arc<P2PGossipSync<Arc<NetworkGraph<Arc<L>>>, Arc<C>, Arc<L>>>,
    Arc<LampoArcOnionMessenger<L>>,
    Arc<L>,
    IgnoringMessageHandler,
    Arc<KeysManager>,
>;

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
    channel_manager: Option<Arc<LampoChannelManager>>,
    conf: LampoConf,
    logger: Arc<LampoLogger>,
}

impl LampoPeerManager {
    pub fn new(conf: &LampoConf, logger: Arc<LampoLogger>) -> LampoPeerManager {
        LampoPeerManager {
            peer_manager: None,
            conf: conf.to_owned(),
            logger,
            channel_manager: None,
        }
    }

    pub fn manager(&self) -> Arc<InnerLampoPeerManager> {
        let manager = self.peer_manager.clone().unwrap();
        manager
    }

    pub fn init(
        &mut self,
        onchain_manager: Arc<LampoChainManager>,
        wallet_manager: Arc<dyn WalletManager>,
        channel_manager: Arc<LampoChannelManager>,
    ) -> error::Result<()> {
        let ephemeral_bytes = [0; 32];
        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let onion_messenger = Arc::new(OnionMessenger::new(
            wallet_manager.ldk_keys().keys_manager.clone(),
            wallet_manager.ldk_keys().keys_manager.clone(),
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
            custom_message_handler: IgnoringMessageHandler {},
        };

        let peer_manager = InnerLampoPeerManager::new(
            lightning_msg_handler,
            current_time.try_into().unwrap(),
            &ephemeral_bytes,
            channel_manager.logger.clone(),
            wallet_manager.ldk_keys().keys_manager.clone(),
        );
        self.peer_manager = Some(Arc::new(peer_manager));
        self.channel_manager = Some(channel_manager.clone());
        Ok(())
    }

    pub fn run(&self) -> error::Result<()> {
        let listen_port = self.conf.port;
        let Some(ref peer_manager) = self.peer_manager else {
            error::bail!("peer manager is None, at this point this should be not None");
        };
        let peer_manager = peer_manager.clone();
        async_run!(async move {
            let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", listen_port))
                .await
                .unwrap();
            log::info!(target: "lampod", "Preparing inbound connections");
            loop {
                let peer_manager = peer_manager.clone();
                let addr = listener.local_addr().unwrap().to_string();
                log::info!(target: "lampod", "preparing address {addr}");
                let tcp_stream = listener.accept().await.unwrap().0;
                log::info!(target: "lampod", "--------- start server on {addr} -----------");
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
        });
        Ok(())
    }

    pub fn is_connected_with(&self, peer_id: NodeId) -> bool {
        let Some(ref manager) = self.peer_manager else {
            panic!("at this point the peer manager should be known");
        };
        for (node_id, _) in manager.get_peer_node_ids() {
            if node_id == peer_id {
                return true;
            }
        }
        false
    }
}

#[async_trait]
impl PeerEvents for LampoPeerManager {
    async fn handle(&self, event: super::peer_event::PeerEvent) -> error::Result<()> {
        match event {
            peer_event::PeerEvent::Connect(node_id, addr, chan) => {
                let connect = Connect {
                    node_id: node_id.to_string(),
                    addr: addr.ip().to_string(),
                    port: addr.port() as u64,
                };
                self.connect(node_id, addr).await?;
                chan.send(connect)?;
            }
        };
        Ok(())
    }

    async fn connect(&self, node_id: NodeId, host: SocketAddr) -> error::Result<()> {
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

    async fn disconnect(&self, node_id: NodeId) -> error::Result<()> {
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
