//! Lampo test framework.
pub mod prelude {
    pub use clightning_testing::prelude::*;
    pub use clightning_testing::*;
    pub use lampod;
    pub use lampod::async_run;
}

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use clightning_testing::btc::BtcNode;
use clightning_testing::prelude::*;
use tempfile::TempDir;

use lampo_async_jsonrpc::JSONRPCv2;
use lampo_bitcoind::BitcoinCore;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::json;
use lampo_common::model::response;
use lampo_common::model::response::NewAddress;
use lampo_core_wallet::CoreWalletManager;
use lampod::actions::handler::LampoHandler;
use lampod::chain::WalletManager;
use lampod::jsonrpc::channels::json_close_channel;
use lampod::jsonrpc::channels::json_list_channels;
use lampod::jsonrpc::inventory::get_info;
use lampod::jsonrpc::offchain::json_decode_invoice;
use lampod::jsonrpc::offchain::json_invoice;
use lampod::jsonrpc::offchain::json_keysend;
use lampod::jsonrpc::offchain::json_offer;
use lampod::jsonrpc::offchain::json_pay;
use lampod::jsonrpc::onchain::json_funds;
use lampod::jsonrpc::onchain::json_new_addr;
use lampod::jsonrpc::open_channel::json_open_channel;
use lampod::jsonrpc::peer_control::json_connect;
use lampod::jsonrpc::CommandHandler;
use lampod::LampoDaemon;

#[macro_export]
macro_rules! wait {
    ($callback:expr, $timeout:expr) => {{
        let mut success = false;
        for _ in 0..4 {
            let result = $callback();
            if let Err(_) = result {
                std::thread::sleep(std::time::Duration::from_secs($timeout));
                continue;
            }
            success = true;
            break;
        }
        assert!(success, "callback got a timeout");
    }};
    ($callback:expr) => {
        $crate::wait!($callback, 5);
    };
}

pub struct LampoTesting {
    inner: Arc<LampoHandler>,
    root_path: Arc<TempDir>,
    pub port: u64,
    pub wallet: Arc<dyn WalletManager>,
    pub mnemonic: String,
    pub btc: Arc<BtcNode>,
    pub info: response::GetInfo,
}

impl LampoTesting {
    pub fn new(btc: Arc<BtcNode>) -> error::Result<Self> {
        let dir = tempfile::tempdir()?;

        // SAFETY: this should be safe because if the system has no
        // ports it is a bug
        let port = port::random_free_port().unwrap();

        let mut lampo_conf = LampoConf::new(
            // FIXME: this is bad we should wrap this logic
            Some(dir.path().to_string_lossy().to_string()),
            Some(lampo_common::bitcoin::Network::Regtest),
            Some(port.into()),
        )?;
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
        let mut lampo = LampoDaemon::new(lampo_conf.clone(), wallet.clone());
        let node = BitcoinCore::new(
            &format!("127.0.0.1:{}", btc.port),
            &btc.user,
            &btc.pass,
            Arc::new(false),
            Some(1),
        )?;
        lampo.init(Arc::new(node))?;

        // Configuring the JSON RPC over unix
        let lampo = Arc::new(lampo);
        let socket_path = format!("{}/lampod.socket", lampo.root_path());
        // FIXME: This can be an InMemory Handler without any problem
        let server = JSONRPCv2::new(lampo.clone(), &socket_path)?;
        server.add_rpc("getinfo", get_info).unwrap();
        server.add_rpc("connect", json_connect).unwrap();
        server.add_rpc("fundchannel", json_open_channel).unwrap();
        server.add_rpc("newaddr", json_new_addr).unwrap();
        server.add_rpc("channels", json_list_channels).unwrap();
        server.add_rpc("funds", json_funds).unwrap();
        server.add_rpc("invoice", json_invoice).unwrap();
        server.add_rpc("offer", json_offer).unwrap();
        server
            .add_rpc("decode_invoice", json_decode_invoice)
            .unwrap();

        server.add_rpc("pay", json_pay).unwrap();
        server.add_rpc("keysend", json_keysend).unwrap();
        server.add_rpc("close", json_close_channel).unwrap();
        let handler = server.handler();
        let rpc_handler = Arc::new(CommandHandler::new(&lampo_conf)?);
        rpc_handler.set_handler(handler);

        lampo.add_external_handler(rpc_handler)?;

        // run lampo and take the handler over to run commands
        let handler = lampo.handler();
        std::thread::spawn(move || lampo.listen().unwrap().join());
        // wait that lampo starts
        std::thread::sleep(Duration::from_secs(1));

        let info: response::GetInfo = handler.call("getinfo", json::json!({}))?;
        log::info!("ready for integration testing!");
        Ok(Self {
            inner: handler,
            mnemonic,
            port: port.into(),
            wallet,
            btc,
            root_path: Arc::new(dir),
            info,
        })
    }

    pub fn fund_wallet(&self, blocks: u64) -> error::Result<bitcoincore_rpc::bitcoin::Address> {
        use clightning_testing::prelude::bitcoincore_rpc::RpcApi;

        // mine some bitcoin inside the lampo address
        let address: NewAddress = self.lampod().call("newaddr", json::json!({})).unwrap();
        let address = bitcoincore_rpc::bitcoin::Address::from_str(&address.address)
            .unwrap()
            .assume_checked();
        let _ = self
            .btc
            .rpc()
            .generate_to_address(blocks, &address)
            .unwrap();

        wait!(|| {
            let funds: response::Utxos = self.inner.call("funds", json::json!({})).unwrap();
            if !funds.transactions.is_empty() {
                return Ok(());
            }
            Err(())
        });
        Ok(address)
    }

    pub fn lampod(&self) -> Arc<LampoHandler> {
        self.inner.clone()
    }

    pub fn root_path(&self) -> Arc<TempDir> {
        self.root_path.clone()
    }
}
