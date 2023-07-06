//! Lampo test framework.
pub mod prelude {
    pub use cln4rust_testing::prelude::*;
    pub use cln4rust_testing::*;
    pub use lampod::async_run;
}

use std::sync::Arc;

use cln4rust_testing::btc::BtcNode;
use cln4rust_testing::prelude::*;
use lampod::jsonrpc::CommandHandler;
use tempfile::TempDir;

use lampo_bitcoind::BitcoinCore;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_jsonrpc::{Handler, JSONRPCv2};
use lampod::actions::handler::LampoHandler;
use lampod::chain::{LampoWalletManager, WalletManager};
use lampod::jsonrpc::channels::json_list_channels;
use lampod::jsonrpc::inventory::get_info;
use lampod::jsonrpc::onchain::json_funds;
use lampod::jsonrpc::onchain::json_new_addr;
use lampod::jsonrpc::open_channel::json_open_channel;
use lampod::jsonrpc::peer_control::json_connect;
use lampod::LampoDeamon;

pub struct LampoTesting {
    inner: Arc<LampoHandler>,
    wallet: Arc<LampoWalletManager>,
    mnemonic: String,
    root_path: Arc<TempDir>,
}

impl LampoTesting {
    pub fn new(btc: &BtcNode) -> error::Result<Self> {
        let dir = tempfile::tempdir()?;

        let lampo_conf = LampoConf::new(
            dir.path().to_str().unwrap(),
            lampo_common::bitcoin::Network::Regtest,
            port::random_free_port().unwrap().into(),
        );
        let (wallet, mnemonic) = LampoWalletManager::new(Arc::new(lampo_conf.clone()))?;
        let wallet = Arc::new(wallet);
        let mut lampo = LampoDeamon::new(lampo_conf.clone(), wallet.clone());
        let node = BitcoinCore::new(&format!("127.0.0.1:{}", btc.port), &btc.user, &btc.pass)?;
        lampo.init(Arc::new(node))?;

        // Configuring the JSON RPC over unix
        let lampo = Arc::new(lampo);
        let socket_path = format!("{}/lampod.socket", lampo.root_path());
        let server = JSONRPCv2::new(lampo.clone(), &socket_path)?;
        server.add_rpc("getinfo", get_info).unwrap();
        server.add_rpc("connect", json_connect).unwrap();
        server.add_rpc("fundchannel", json_open_channel).unwrap();
        server.add_rpc("newaddr", json_new_addr).unwrap();
        server.add_rpc("channels", json_list_channels).unwrap();
        server.add_rpc("funds", json_funds).unwrap();
        let handler = server.handler();
        let rpc_handler = Arc::new(CommandHandler::new(&lampo_conf.clone())?);
        rpc_handler.set_handler(handler.clone());
        lampo.add_external_handler(rpc_handler.clone())?;

        // run lampo and take the handler over to run commands
        let handler = lampo.handler();
        std::thread::spawn(move || lampo.listen());
        log::info!("ready for integration testing!");
        Ok(Self {
            inner: handler,
            mnemonic,
            wallet: wallet.clone(),
            root_path: Arc::new(dir),
        })
    }

    pub fn lampod(&self) -> Arc<LampoHandler> {
        self.inner.clone()
    }

    pub fn root_path(&self) -> Arc<TempDir> {
        self.root_path.clone()
    }
}
