use std::{sync::Arc, time::SystemTime};

use lightning::ln::peer_handler::SimpleArcPeerManager;
use lightning_net_tokio;
use lightning_net_tokio::SocketDescriptor;

use crate::{chain::LampoChainManager, conf::LampoConf, utils::logger::LampoLogger};

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
}

impl LampoPeerManager {
    pub fn new(conf: &LampoConf) -> LampoPeerManager {
        LampoPeerManager {
            peer_manager: None,
            conf: conf.to_owned(),
        }
    }

    pub fn manager(&self) -> Arc<InnerLampoPeerManager> {
        let manager = self.peer_manager.clone().unwrap();
        manager
    }

    pub fn init(
        &self,
        onchain_manager: &Arc<LampoChainManager>,
        channel_manager: &Arc<LampoChannelManager>,
    ) -> Result<(), ()> {
        let mut ephemeral_bytes = [0; 32];
        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        /*FIXME: implement this
        let lightning_msg_handler = MessageHandler {
            chan_handler: &channel_manager.channeld.unwrap(),
        };
        let ignoring_custom_msg_handler = IgnoringMessageHandler {};
        let peer_manager = PeerManager::new(
            lightning_msg_handler,
            current_time.try_into().unwrap(),
            &ephemeral_bytes,
            &channel_manager.logger.as_ref().clone(),
            &ignoring_custom_msg_handler,
            onchain_manager.keymanager.inner(),
        );
        self.peer_manager = Some(peer_manager);
        */
        Ok(())
    }

    pub async fn run(self) -> Result<(), ()> {
        let listen_port = self.conf.port;
        let Some(peer_manager) = self.peer_manager else {
            return Err(())
        };
        let peer_manager = peer_manager.clone();
        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", listen_port))
            .await
            .unwrap();
        loop {
            let peer_manager = peer_manager.clone();
            let tcp_stream = listener.accept().await.unwrap().0;
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
