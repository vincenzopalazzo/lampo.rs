use std::net::SocketAddr;
use std::time::Duration;
use std::{sync::Arc, time::SystemTime};

use async_trait::async_trait;

use lampo_common::bitcoin;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::ldk;
use lampo_common::ldk::ln::peer_handler::MessageHandler;
use lampo_common::ldk::ln::peer_handler::{IgnoringMessageHandler, PeerManager};
use lampo_common::ldk::net;
use lampo_common::ldk::net::SocketDescriptor;
use lampo_common::ldk::onion_message::messenger::{MessageRouter, OnionMessenger};
use lampo_common::ldk::routing::gossip::{NetworkGraph, P2PGossipSync};
use lampo_common::ldk::sign::KeysManager;
use lampo_common::model::Connect;
use lampo_common::types::NodeId;

use crate::async_run;
use crate::chain::{LampoChainManager, WalletManager};
use crate::ln::LampoChannelManager;
use crate::utils::logger::LampoLogger;

use super::channel_manager::{LampoArcChannelManager, LampoChainMonitor};
use super::events::PeerEvents;
use super::peer_event;

pub struct FakeMsgRouter;

impl MessageRouter for FakeMsgRouter {
    fn find_path(
        &self,
        _: bitcoin::secp256k1::PublicKey,
        _: Vec<bitcoin::secp256k1::PublicKey>,
        _: ldk::onion_message::messenger::Destination,
    ) -> Result<ldk::onion_message::messenger::OnionMessagePath, ()> {
        log::warn!("ingoring the find path in the message router");
        Err(())
    }

    fn create_blinded_paths<
        T: lampo_common::secp256k1::Signing + lampo_common::secp256k1::Verification,
    >(
        &self,
        _recipient: lampo_common::secp256k1::PublicKey,
        _peers: Vec<lampo_common::secp256k1::PublicKey>,
        _secp_ctx: &lampo_common::secp256k1::Secp256k1<T>,
    ) -> Result<Vec<ldk::blinded_path::BlindedPath>, ()> {
        unimplemented!()
    }
}

pub type LampoArcOnionMessenger<L> = OnionMessenger<
    Arc<KeysManager>,
    Arc<KeysManager>,
    Arc<L>,
    Arc<FakeMsgRouter>,
    IgnoringMessageHandler,
    IgnoringMessageHandler,
>;

pub type SimpleArcPeerManager<M, T, L> = PeerManager<
    SocketDescriptor,
    Arc<LampoArcChannelManager<M, T, T, L>>,
    Arc<P2PGossipSync<Arc<NetworkGraph<Arc<L>>>, Arc<T>, Arc<L>>>,
    Arc<LampoArcOnionMessenger<L>>,
    Arc<L>,
    IgnoringMessageHandler,
    Arc<KeysManager>,
>;

type InnerLampoPeerManager =
    SimpleArcPeerManager<LampoChainMonitor, LampoChainManager, LampoLogger>;

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
        self.peer_manager.clone().unwrap()
    }

    pub fn init(
        &mut self,
        _onchain_manager: Arc<LampoChainManager>,
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
            Arc::new(FakeMsgRouter {}),
            IgnoringMessageHandler {},
            IgnoringMessageHandler {},
        ));

        let gossip_sync = Arc::new(P2PGossipSync::new(
            channel_manager.graph(),
            None::<Arc<LampoChainManager>>,
            self.logger.clone(),
        ));

        let lightning_msg_handler = MessageHandler {
            chan_handler: channel_manager.channeld.clone().unwrap(),
            onion_message_handler: onion_messenger,
            route_handler: gossip_sync,
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
        std::thread::spawn(move || {
            async_run!(async move {
                let bind_addr = format!("0.0.0.0:{}", listen_port);
                log::info!(target: "lampo", "Litening for in-bound connection on {bind_addr}");
                let listener = tokio::net::TcpListener::bind(bind_addr).await.unwrap();
                loop {
                    let peer_manager = peer_manager.clone();
                    let tcp_stream = listener.accept().await.unwrap().0;
                    log::info!(target: "lampo", "Got new connection {}", tcp_stream.peer_addr().unwrap());
                    let _ = tokio::spawn(async move {
                        // Use LDK's supplied networking battery to facilitate inbound
                        // connections.
                        net::setup_inbound(peer_manager.clone(), tcp_stream.into_std().unwrap())
                            .await;
                    })
                    .await;
                }
            });
        });
        Ok(())
    }

    pub fn is_connected_with(&self, peer_id: NodeId) -> bool {
        let Some(ref manager) = self.peer_manager else {
            panic!("at this point the peer manager should be known");
        };
        manager.peer_by_node_id(&peer_id).is_some()
    }
}

#[async_trait]
impl PeerEvents for LampoPeerManager {
    async fn handle(&self, event: super::peer_event::PeerCommand) -> error::Result<()> {
        match event {
            peer_event::PeerCommand::Connect(node_id, addr, chan) => {
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
        let Some(close_callback) = net::connect_outbound(self.manager(), node_id, host).await
        else {
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
            match manager.peer_by_node_id(&node_id) {
                Some(_) => return Ok(()),
                None => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    }

    async fn disconnect(&self, node_id: NodeId) -> error::Result<()> {
        //check the pubkey matches a valid connected peer
        if self.manager().peer_by_node_id(&node_id).is_none() {
            error::bail!("Error: Could not find peer `{node_id}`");
        }

        self.manager().disconnect_by_node_id(node_id);
        Ok(())
    }
}
