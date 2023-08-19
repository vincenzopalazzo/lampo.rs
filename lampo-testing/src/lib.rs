//! Lampo test framework.
pub mod prelude {
    pub use cln4rust_testing::prelude::*;
    pub use cln4rust_testing::*;
    pub use lampod;
    pub use lampod::async_run;
}

use std::sync::Arc;
use std::time::Duration;

use cln4rust_testing::btc::BtcNode;
use cln4rust_testing::prelude::*;
use tempfile::TempDir;

use lampo_bitcoind::BitcoinCore;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_core_wallet::CoreWalletManager;
use lampo_jsonrpc::JSONRPCv2;
use lampod::actions::handler::LampoHandler;
use lampod::chain::WalletManager;
use lampod::jsonrpc::channels::json_list_channels;
use lampod::jsonrpc::inventory::get_info;
use lampod::jsonrpc::offchain::json_decode_invoice;
use lampod::jsonrpc::offchain::json_invoice;
use lampod::jsonrpc::offchain::json_pay;
use lampod::jsonrpc::onchain::json_funds;
use lampod::jsonrpc::onchain::json_new_addr;
use lampod::jsonrpc::open_channel::json_open_channel;
use lampod::jsonrpc::peer_control::json_connect;
use lampod::jsonrpc::CommandHandler;
use lampod::LampoDeamon;

#[macro_export]
macro_rules! wait {
    ($callback:expr, $timeout:expr) => {{
        let mut success = false;
        for wait in 0..$timeout {
            let result = $callback();
            if let Err(err) = result {
                log::debug!("callback return {:?}", err);
                std::thread::sleep(std::time::Duration::from_millis(wait));
                continue;
            }
            log::info!("callback completed in {wait} milliseconds");
            success = true;
            break;
        }
        assert!(success, "callback got a timeout");
    }};
    ($callback:expr) => {
        $crate::wait!($callback, 50);
    };
}

pub struct LampoTesting {
    inner: Arc<LampoHandler>,
    root_path: Arc<TempDir>,
    pub port: u64,
    pub wallet: Arc<dyn WalletManager>,
    pub mnemonic: String,
}

impl LampoTesting {
    pub fn new(btc: &BtcNode) -> error::Result<Self> {
        let dir = tempfile::tempdir()?;

        // SAFETY: this should be safe because if the system has no
        // ports it is a bug
        let port = port::random_free_port().unwrap();

        let mut lampo_conf = LampoConf::new(
            dir.path().to_str().unwrap(),
            lampo_common::bitcoin::Network::Regtest,
            port.into(),
        );
        let core_url = format!("127.0.0.1:{}", btc.port);
        lampo_conf.core_pass = Some(btc.pass.clone());
        lampo_conf.core_url = Some(core_url);
        lampo_conf.core_user = Some(btc.user.clone());
        lampo_conf
            .ldk_conf
            .channel_handshake_limits
            .force_announced_channel_preference = false;
        let (wallet, mnemonic) = CoreWalletManager::new(Arc::new(lampo_conf.clone()))?;
        let wallet = Arc::new(wallet);
        let mut lampo = LampoDeamon::new(lampo_conf.clone(), wallet.clone());
        let node = BitcoinCore::new(
            &format!("127.0.0.1:{}", btc.port),
            &btc.user,
            &btc.pass,
            Arc::new(false),
            Some(5),
        )?;
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
        server.add_rpc("invoice", json_invoice).unwrap();
        server
            .add_rpc("decode_invoice", json_decode_invoice)
            .unwrap();

        server.add_rpc("pay", json_pay).unwrap();
        let handler = server.handler();
        let rpc_handler = Arc::new(CommandHandler::new(&lampo_conf)?);
        rpc_handler.set_handler(handler);
        lampo.add_external_handler(rpc_handler)?;

        // run lampo and take the handler over to run commands
        let handler = lampo.handler();
        std::thread::spawn(move || lampo.listen().unwrap().join());
        // wait that lampo starts
        std::thread::sleep(Duration::from_secs(1));
        log::info!("ready for integration testing!");
        Ok(Self {
            inner: handler,
            mnemonic,
            port: port.into(),
            wallet,
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
