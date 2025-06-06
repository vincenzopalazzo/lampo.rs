//! Lampo daemon implementation.
//!
//! Welcome to the Lampo daemon codebase.
//! This is the core part of the code responsible for interacting
//! with the Lampo Lightning node.
//!
//! This codebase also contains documentation with numerous
//! design pattern references that we have used to design
//! the Lampo node. We hope that this documentation will
//! help you understand our design philosophy better.
//!
//! Have fun exploring the code!
pub mod actions;
mod builtin;
pub mod chain;
pub mod command;
pub mod jsonrpc;
pub mod ln;
pub mod persistence;

use std::cell::Cell;
use std::sync::Arc;
use std::time::SystemTime;

use tokio::task::JoinHandle;

use lampo_common::backend::Backend;
use lampo_common::conf::LampoConf;
use lampo_common::handler::ExternalHandler;
use lampo_common::json;
use lampo_common::ldk::events::{Event, ReplayEvent};
use lampo_common::ldk::io;
use lampo_common::ldk::processor::{process_events_async, BackgroundProcessor, GossipSync};
use lampo_common::types::LampoGraph;
use lampo_common::utils;
use lampo_common::wallet::WalletManager;
use lampo_common::{error, ldk};

use crate::actions::handler::LampoHandler;
use crate::actions::Handler;
use crate::chain::LampoChainManager;
use crate::ln::OffchainManager;
use crate::ln::{LampoChannelManager, LampoInventoryManager, LampoPeerManager};
use crate::persistence::LampoPersistence;
use crate::utils::logger::LampoLogger;

pub(crate) type P2PGossipSync =
    ldk::routing::gossip::P2PGossipSync<Arc<LampoGraph>, Arc<LampoChainManager>, Arc<LampoLogger>>;

/// LampoDaemon is the main data structure that uses the facade
/// pattern to hide the complexity of the LDK library. You can interact
/// with the LampoDaemon's components through access
/// methods (similar to get methods in modern procedural languages).
///
/// Another way to view the LampoDaemon is as
/// a microkernel pattern, especially for developers
/// who are interested in building their own node on
/// top of the LampoDaemon.
#[repr(C)]
pub struct LampoDaemon {
    conf: Arc<LampoConf>,
    peer_manager: Option<Arc<LampoPeerManager>>,
    onchain_manager: Option<Arc<LampoChainManager>>,
    channel_manager: Option<Arc<LampoChannelManager>>,
    inventory_manager: Option<Arc<LampoInventoryManager>>,
    wallet_manager: Arc<dyn WalletManager>,
    offchain_manager: Option<Arc<OffchainManager>>,
    logger: Arc<LampoLogger>,
    persister: Arc<LampoPersistence>,
    handler: Option<Arc<LampoHandler>>,
    process: Cell<Option<BackgroundProcessor>>,
}

unsafe impl Send for LampoDaemon {}
unsafe impl Sync for LampoDaemon {}

impl LampoDaemon {
    pub fn new(config: Arc<LampoConf>, wallet_manager: Arc<dyn WalletManager>) -> Self {
        let root_path = config.path();
        LampoDaemon {
            conf: config,
            logger: Arc::new(LampoLogger {}),
            persister: Arc::new(LampoPersistence::new(root_path.into())),
            peer_manager: None,
            onchain_manager: None,
            channel_manager: None,
            inventory_manager: None,
            wallet_manager,
            offchain_manager: None,
            handler: None,
            process: Cell::new(None),
        }
    }

    pub fn root_path(&self) -> String {
        self.conf.path()
    }

    pub fn conf(&self) -> Arc<LampoConf> {
        self.conf.clone()
    }

    pub fn init_onchaind(&mut self, client: Arc<dyn Backend>) -> error::Result<()> {
        log::debug!(target: "lampod", "init onchaind ..");
        let onchain_manager = LampoChainManager::new(client, self.wallet_manager.clone());
        self.onchain_manager = Some(Arc::new(onchain_manager));
        Ok(())
    }

    pub fn onchain_manager(&self) -> Arc<LampoChainManager> {
        self.onchain_manager.clone().unwrap()
    }

    pub async fn init_channeld(&mut self) -> error::Result<()> {
        log::debug!(target: "lampod", "init channeld ...");
        let manager = LampoChannelManager::new(
            &self.conf,
            self.logger.clone(),
            self.onchain_manager(),
            self.wallet_manager.clone(),
            self.persister.clone(),
        );
        self.channel_manager = Some(Arc::new(manager));
        self.channel_manager().listen().await?;
        Ok(())
    }

    pub fn channel_manager(&self) -> Arc<LampoChannelManager> {
        self.channel_manager.clone().unwrap()
    }

    pub fn offchain_manager(&self) -> Arc<OffchainManager> {
        self.offchain_manager.clone().unwrap()
    }

    pub fn init_offchain_manager(&mut self) -> error::Result<()> {
        log::debug!(target: "lampod", "init offchain manager ...");
        let manager = OffchainManager::new(
            self.wallet_manager().ldk_keys().keys_manager.clone(),
            self.channel_manager(),
            self.logger.clone(),
            self.conf.clone(),
            self.onchain_manager(),
        )?;
        self.offchain_manager = Some(Arc::new(manager));
        Ok(())
    }

    pub fn init_peer_manager(&mut self) -> error::Result<()> {
        log::debug!(target: "lampo", "init peer manager ...");
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
        self.peer_manager.clone().unwrap()
    }

    fn init_inventory_manager(&mut self) -> error::Result<()> {
        log::debug!(target: "lampod", "init inventory manager ...");
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
        log::debug!(target: "lampod", "init inventory manager ...");
        let handler = LampoHandler::new(self);
        self.handler = Some(Arc::new(handler));
        Ok(())
    }

    pub fn handler(&self) -> Arc<LampoHandler> {
        self.handler.clone().unwrap()
    }

    pub fn init_reactor(&mut self) -> error::Result<()> {
        Ok(())
    }

    pub async fn init(&mut self, client: Arc<dyn Backend>) -> error::Result<()> {
        log::debug!(target: "lampod", "init lampod ...");
        self.init_onchaind(client.clone())?;
        self.init_channeld().await?;
        self.init_offchain_manager()?;
        self.init_peer_manager()?;
        self.init_inventory_manager()?;
        self.init_event_handler()?;
        client.set_handler(self.handler());
        client.set_channel_manager(self.channel_manager().manager());
        client.set_chain_monitor(self.channel_manager().chain_monitor());
        self.channel_manager().set_handler(self.handler());
        Ok(())
    }

    /// Registers an external handler to handle incoming requests from external sources.
    /// These requests are passed to the handler via the `call` method.
    ///
    /// Additionally, the registered handler serves as the entry point for
    /// the Chain of Responsibility pattern that handles all unsupported commands that the Lampod daemon
    /// may receive from external sources (assuming the user has defined a handler for them).
    pub async fn add_external_handler(
        &self,
        ext_handler: Arc<dyn ExternalHandler>,
    ) -> error::Result<()> {
        let Some(ref handler) = self.handler else {
            error::bail!("Initial handler is None");
        };
        handler.add_external_handler(ext_handler).await?;
        Ok(())
    }

    pub fn listen(self: Arc<Self>) -> JoinHandle<Result<(), io::Error>> {
        log::info!(target: "lampod", "Starting lightning node version `{}`", env!("CARGO_PKG_VERSION"));
        let gossip_sync: Arc<P2PGossipSync> = Arc::new(ldk::routing::gossip::P2PGossipSync::new(
            self.channel_manager().graph(),
            None::<Arc<LampoChainManager>>,
            self.logger.clone(),
        ));

        log::info!(target: "lampo", "Stating onchaind");
        let _ = self.onchain_manager().listen();
        log::info!(target: "lampo", "Starting peer manager");
        let _ = self.peer_manager().run();
        log::info!(target: "lampo", "Starting channel manager");
        let _ = self.channel_manager().listen();

        tokio::spawn(async move {
            process_events_async(
                self.persister.clone(),
                |env| self.handler_ldk_events(env),
                self.channel_manager().chain_monitor(),
                self.channel_manager().manager(),
                Some(self.peer_manager().onion_messager()),
                GossipSync::p2p(gossip_sync),
                self.peer_manager().manager(),
                self.logger.clone(),
                Some(self.channel_manager().scorer()),
                |d| {
                    Box::pin(async move {
                        tokio::time::sleep(d).await;
                        // if we return true, ldk is going to stop the processor
                        // so we should use this when we have the stop command
                        false
                    })
                },
                false,
                || {
                    Some(
                        SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap(),
                    )
                },
            )
            .await
            // FIXME: add the stop event handler
        })
    }

    // FIXME: what about replay event?
    async fn handler_ldk_events(&self, env: Event) -> Result<(), ReplayEvent> {
        if let Err(err) = self.handler().handle(env).await {
            log::error!(target: "lampod", "Error handling event: {:?}", err);
        }
        Ok(())
    }

    /// Call any method supported by the lampod configuration. This includes
    /// a lot of handler code. This function serves as a broker pattern in some ways,
    /// but it may also function as a chain of responsibility pattern in certain cases.
    ///
    /// Welcome to the third design pattern in under 300 lines of code. The code will clarify the
    /// idea, but be prepared to see a broker pattern begin as a chain of responsibility pattern
    /// at some point.
    pub async fn call(&self, method: &str, args: json::Value) -> error::Result<json::Value> {
        let Some(ref handler) = self.handler else {
            error::bail!("at this point the handler should be not None");
        };
        handler.call::<json::Value, json::Value>(method, args).await
    }
}
