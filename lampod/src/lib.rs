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
pub mod handler;
pub mod jsonrpc;
pub mod ln;
pub mod persistence;
pub mod utils;

use std::cell::Cell;
use std::sync::Arc;
use std::thread::JoinHandle;

use tokio::runtime::Runtime;

use lampo_common::backend::Backend;
use lampo_common::bitcoin::locktime::Height;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::json;
use lampo_common::ldk::events::Event;
use lampo_common::ldk::processor::{BackgroundProcessor, GossipSync};
use lampo_common::ldk::routing::gossip::P2PGossipSync;
use lampo_common::wallet::WalletManager;

use crate::actions::handler::LampoHandler;
use crate::actions::Handler;
use crate::chain::LampoChainManager;
use crate::handler::external_handler::ExternalHandler;
use crate::ln::OffchainManager;
use crate::ln::{LampoChannelManager, LampoInventoryManager, LampoPeerManager};
use crate::persistence::LampoPersistence;
use crate::utils::logger::LampoLogger;

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
pub struct LampoDeamon {
    conf: LampoConf,
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

    // FIXME: remove this
    rt: Runtime,
}

unsafe impl Send for LampoDeamon {}
unsafe impl Sync for LampoDeamon {}

impl LampoDeamon {
    pub fn new(config: LampoConf, wallet_manager: Arc<dyn WalletManager>) -> Self {
        let root_path = config.path();
        //FIXME: sync some where else
        let wallet = wallet_manager.clone();
        let _ = std::thread::spawn(move || wallet.sync().unwrap());
        LampoDeamon {
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
        log::debug!(target: "lampod", "init onchaind ..");
        let onchain_manager = LampoChainManager::new(client, self.wallet_manager.clone());
        self.onchain_manager = Some(Arc::new(onchain_manager));
        Ok(())
    }

    pub fn onchain_manager(&self) -> Arc<LampoChainManager> {
        self.onchain_manager.clone().unwrap()
    }

    pub fn init_channeld(&mut self) -> error::Result<()> {
        log::debug!(target: "lampod", "init channeld ...");
        let mut manager = LampoChannelManager::new(
            &self.conf,
            self.logger.clone(),
            self.onchain_manager(),
            self.wallet_manager.clone(),
            self.persister.clone(),
        );
        let (block_hash, height) = self.onchain_manager().backend.get_best_block()?;
        let block = self.onchain_manager().backend.get_block(&block_hash)?;
        let timestamp = match block {
            lampo_common::backend::BlockData::FullBlock(block) => block.header.time,
            lampo_common::backend::BlockData::HeaderOnly(header) => header.time,
        };

        let height = height.ok_or(error::anyhow!("height not present"))?;

        if manager.is_restarting()? {
            manager.restart()?;
        } else {
            manager.start(block_hash, Height::from_consensus(height)?, timestamp)?;
        }

        self.channel_manager = Some(Arc::new(manager));
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
            Arc::new(self.conf.clone()),
            self.onchain_manager(),
        )?;
        self.offchain_manager = Some(Arc::new(manager));
        Ok(())
    }

    pub fn init_peer_manager(&mut self) -> error::Result<()> {
        log::debug!(target: "lampod", "init peer manager ...");
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

    pub fn init(&mut self, client: Arc<dyn Backend>) -> error::Result<()> {
        log::debug!(target: "lampod", "init lampod ...");
        self.init_onchaind(client.clone())?;
        self.init_channeld()?;
        self.init_offchain_manager()?;
        self.init_peer_manager()?;
        self.init_inventory_manager()?;
        self.init_event_handler()?;
        client.set_handler(self.handler());
        self.channel_manager().set_handler(self.handler());
        Ok(())
    }

    /// Registers an external handler to handle incoming requests from external sources.
    /// These requests are passed to the handler via the `call` method.
    ///
    /// Additionally, the registered handler serves as the entry point for
    /// the Chain of Responsibility pattern that handles all unsupported commands that the Lampod daemon
    /// may receive from external sources (assuming the user has defined a handler for them).
    pub fn add_external_handler(&self, ext_handler: Arc<dyn ExternalHandler>) -> error::Result<()> {
        let Some(ref handler) = self.handler else {
            error::bail!("Initial handler is None");
        };
        handler.add_external_handler(ext_handler)?;
        Ok(())
    }

    pub fn listen(self: Arc<Self>) -> error::Result<JoinHandle<std::io::Result<()>>> {
        log::info!(target: "lampod", "Starting lightning node version `{}`", env!("CARGO_PKG_VERSION"));
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
            GossipSync::p2p(gossip_sync),
            self.peer_manager().manager(),
            self.logger.clone(),
            Some(self.channel_manager().scorer()),
        );

        let _ = self.onchain_manager().backend.clone().listen();
        let _ = self.channel_manager().listen();
        let _ = self.peer_manager().run();
        Ok(std::thread::spawn(move || {
            let _ = background_processor.join();
            Ok(())
        }))
    }

    /// Call any method supported by the lampod configuration. This includes
    /// a lot of handler code. This function serves as a broker pattern in some ways,
    /// but it may also function as a chain of responsibility pattern in certain cases.
    ///
    /// Welcome to the third design pattern in under 300 lines of code. The code will clarify the
    /// idea, but be prepared to see a broker pattern begin as a chain of responsibility pattern
    /// at some point.
    pub fn call(&self, method: &str, args: json::Value) -> error::Result<json::Value> {
        let Some(ref handler) = self.handler else {
            error::bail!("at this point the handler should be not None");
        };
        handler.call::<json::Value, json::Value>(method, args)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Arc;

    use clightningrpc_conf::CLNConf;

    use lampo_bdk_wallet::BDKWalletManager;
    use lampo_common::bitcoin;
    use lampo_common::conf::LampoConf;
    use lampo_common::json;
    use lampo_common::ldk::util::config::UserConfig;
    use lampo_common::logger;
    use lampo_common::model::request;
    use lampo_common::secp256k1;
    use lampo_common::wallet::WalletManager;
    use lampo_nakamoto::{Config, Network};

    use crate::{async_run, ln::events::PeerEvents, LampoDeamon};

    #[test]
    fn simple_node_connection() {
        logger::init("debug", None).expect("initializing logger for the first time");
        let conf = LampoConf {
            ldk_conf: UserConfig::default(),
            network: bitcoin::Network::Testnet,
            port: 19753,
            root_path: "/tmp".to_string(),
            inner: Some(CLNConf::new("/tmp/".to_owned(), true)),
            private_key: None,
            channels_keys: None,
            node: String::from("nakamoto"),
            core_pass: None,
            core_url: None,
            core_user: None,
        };
        let (wallet, _) = BDKWalletManager::new(conf.clone().into()).unwrap();
        let mut lampo = LampoDeamon::new(conf, Arc::new(wallet));

        let mut conf = Config::default();
        conf.network = Network::Testnet;
        let client = Arc::new(lampo_nakamoto::Nakamoto::new(conf).unwrap());

        let result = lampo.init(client);
        assert!(result.is_ok(), "{:?}", result);

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
                .connect(connect.node_id(), connect.addr().unwrap())
        );
        assert!(result.is_ok(), "{:?}", result);
    }

    #[test]
    fn simple_get_info() {
        let conf = LampoConf {
            ldk_conf: UserConfig::default(),
            network: bitcoin::Network::Testnet,
            port: 19753,
            root_path: "/tmp".to_string(),
            inner: Some(CLNConf::new("/tmp/".to_owned(), true)),
            private_key: None,
            channels_keys: None,
            node: String::from("nakamoto"),
            core_pass: None,
            core_url: None,
            core_user: None,
        };
        conf.prepare_dirs().unwrap();
        let (wallet, _) = BDKWalletManager::new(conf.clone().into()).unwrap();
        let mut lampo = LampoDeamon::new(conf, Arc::new(wallet));

        let mut conf = Config::default();
        conf.network = Network::Testnet;
        let client = Arc::new(lampo_nakamoto::Nakamoto::new(conf).unwrap());

        let result = lampo.init(client);
        assert!(result.is_ok(), "{:?}", result);
        let payload = json::json!({});
        let result = lampo.call("getinfo", payload);
        assert!(result.is_ok(), "{:?}", result);
    }

    #[test]
    #[ignore = "ignoring due a nakamoto crash"]
    fn lampod_from_privatekey() {
        let key = secp256k1::SecretKey::from_str(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();

        let mut last_node_id: Option<String> = None;
        for i in 0..2 {
            let conf = LampoConf {
                ldk_conf: UserConfig::default(),
                network: bitcoin::Network::Testnet,
                port: 19753,
                root_path: format!("/tmp/lampo-{i}"),
                inner: Some(CLNConf::new("/tmp/".to_owned(), true)),
                private_key: None,
                channels_keys: None,

                node: String::from("nakamoto"),
                core_pass: None,
                core_url: None,
                core_user: None,
            };
            conf.prepare_dirs().unwrap();
            let key = bitcoin::PrivateKey::new(key, conf.network);
            let wallet = BDKWalletManager::try_from((key, None)).unwrap();

            let mut lampo = LampoDeamon::new(conf, Arc::new(wallet));

            let mut conf = Config::default();
            conf.network = Network::Testnet;
            let client = Arc::new(lampo_nakamoto::Nakamoto::new(conf).unwrap());

            let result = lampo.init(client);
            assert!(result.is_ok());
            let payload = json::json!({});
            let result = lampo.call("getinfo", payload);
            if last_node_id.is_none() {
                last_node_id = result.unwrap().get("node_id").map(|i| i.to_string());
            } else {
                let node_id = result.unwrap().get("node_id").map(|i| i.to_string());
                assert_eq!(node_id, last_node_id);
            }
        }
    }

    #[test]
    #[ignore = "ingnoring due a nakamoto crash"]
    fn lampod_from_privatekey_with_channels_keys() {
        let key = secp256k1::SecretKey::from_str(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        let channel_keys = "0000000000000000000000000000000000000000000000000000000000000010/0000000000000000000000000000000000000000000000000000000000000010/0000000000000000000000000000000000000000000000000000000000000011/0000000000000000000000000000000000000000000000000000000000000012/0000000000000000000000000000000000000000000000000000000000000013/0000000000000000000000000000000000000000000000000000000000000014/FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";

        let mut last_node_id: Option<String> = None;
        for i in 0..2 {
            let conf = LampoConf {
                ldk_conf: UserConfig::default(),
                network: bitcoin::Network::Testnet,
                port: 19753,
                root_path: format!("/tmp/lampo-{i}"),
                inner: Some(CLNConf::new("/tmp/".to_owned(), true)),
                private_key: None,
                channels_keys: None,

                node: String::from("nakamoto"),
                core_pass: None,
                core_url: None,
                core_user: None,
            };
            let key = bitcoin::PrivateKey::new(key, conf.network);
            let wallet = BDKWalletManager::try_from((key, Some(channel_keys.to_string()))).unwrap();

            let mut lampo = LampoDeamon::new(conf, Arc::new(wallet));

            let mut conf = Config::default();
            conf.network = Network::Testnet;
            let client = Arc::new(lampo_nakamoto::Nakamoto::new(conf).unwrap());

            let result = lampo.init(client);
            assert!(result.is_ok());
            let payload = json::json!({});
            let result = lampo.call("getinfo", payload);
            if last_node_id.is_none() {
                last_node_id = result.unwrap().get("node_id").map(|i| i.to_string());
            } else {
                let node_id = result.unwrap().get("node_id").map(|i| i.to_string());
                assert_eq!(node_id, last_node_id);
            }
        }
    }
}
