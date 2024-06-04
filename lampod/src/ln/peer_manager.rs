use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;

use async_trait::async_trait;

use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::ldk;
use lampo_common::ldk::blinded_path::EmptyNodeIdLookUp;
use lampo_common::ldk::ln::peer_handler::MessageHandler;
use lampo_common::ldk::ln::peer_handler::{IgnoringMessageHandler, PeerManager};
use lampo_common::ldk::net;
use lampo_common::ldk::net::SocketDescriptor;
use lampo_common::ldk::onion_message::messenger::OnionMessenger;
use lampo_common::ldk::routing::gossip::P2PGossipSync;
use lampo_common::ldk::sign::KeysManager;
use lampo_common::model::Connect;
use lampo_common::types::NodeId;

use crate::async_run;
use crate::chain::{LampoChainManager, WalletManager};
use crate::ln::LampoChannelManager;
use crate::utils::logger::LampoLogger;

use super::channel_manager::{LampoArcChannelManager, LampoChainMonitor, LampoGraph};
use super::events::PeerEvents;
use super::onion_message::LampoMsgRouter;
use super::peer_event;

pub type LampoArcOnionMessenger<L> = OnionMessenger<
    Arc<KeysManager>,
    Arc<KeysManager>,
    Arc<L>,
    EmptyNodeIdLookUp,
    Arc<LampoMsgRouter<Arc<LampoGraph>, Arc<LampoLogger>, Arc<KeysManager>>>,
    IgnoringMessageHandler,
    IgnoringMessageHandler,
>;

pub type SimpleArcPeerManager<M, T, L> = PeerManager<
    SocketDescriptor,
    Arc<LampoArcChannelManager<M, T, T, L>>,
    Arc<P2PGossipSync<Arc<LampoGraph>, Arc<T>, Arc<L>>>,
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
            EmptyNodeIdLookUp {},
            Arc::new(LampoMsgRouter::new(
                channel_manager.graph(),
                wallet_manager.ldk_keys().keys_manager.clone(),
            )?),
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
        let chan_manager = self
            .channel_manager
            .clone()
            .ok_or(error::anyhow!("channel manager is None"))?;
        let alias = self.conf.alias.clone().unwrap_or_default();
        let addr = self
            .conf
            .announce_addr
            .clone()
            .unwrap_or_else(|| "127.0.0.1".to_string());
        std::thread::spawn(move || {
            let result = async_run!(async move {
                let bind_addr = format!("{addr}:{listen_port}");
                log::info!(target: "lampo", "Listening for in-bound connection on {bind_addr}");
                let listener = match tokio::net::TcpListener::bind(bind_addr.clone()).await {
                    Ok(listener) => listener,
                    Err(e) => {
                        return Err::<(), _>(error::anyhow!("Error binding to address: {}", e));
                    }
                };

                loop {
                    let alias = alias.clone();
                    let peer_manager = peer_manager.clone();
                    let chan_manager = chan_manager.clone();
                    let accept = listener.accept().await;
                    let accept = accept
                        .map_err(|err| error::anyhow!("Error accepting connection: {}", err))?;
                    match accept {
                        (tcp_stream, _) => {
                            log::info!(target: "lampo", "Got new connection {}", tcp_stream.peer_addr().unwrap());
                            let addr = bind_addr.clone();
                            let _ = tokio::spawn(async move {
                                // Use LDK's supplied networking battery to facilitate inbound
                                // connections.
                                net::setup_inbound(
                                    peer_manager.clone(),
                                    tcp_stream.into_std().expect("impossible to convert a tpc_stream from tokio to std"),
                                )
                                .await;

                                // Then, update our announcement once an hour to keep it fresh but avoid unnecessary churn
                                // in the global gossip network.
                                // FIXME: this value should be possible to alterate from config
                                let mut interval = tokio::time::interval(Duration::from_secs(1));
                                loop {
                                    interval.tick().await;
                                    // Don't bother trying to announce if we don't have any public channls, though our
                                    // peers should drop such an announcement anyway. Note that announcement may not
                                    // propagate until we have a channel with 6+ confirmations.
                                    if chan_manager
                                        .manager()
                                        .list_channels()
                                        .iter()
                                        .any(|chan| chan.is_public)
                                    {
                                        peer_manager.broadcast_node_announcement(
                                            [0; 3],
                                            alias.as_bytes().try_into().unwrap_or([0u8; 32]),
                                            vec![ldk::ln::msgs::SocketAddress::from_str(&addr)
                                                .expect("impossible to convert an addr to ln socket addr (wire format)")],
                                        );
                                    }
                                }
                            })
                            .await;
                        }
                    }
                }
            });

            if let Err(err) = &result {
                log::error!("error while try to listen on inbound connection: `{err}`");
            }
            result
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
