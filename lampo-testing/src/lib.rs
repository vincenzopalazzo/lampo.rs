//! Lampo test framework.
pub mod prelude {
    pub use clightning_testing::prelude::*;
    pub use clightning_testing::*;
    pub use lampod;
}

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use clightning_testing::btc::BtcNode;
use clightning_testing::prelude::*;
use lampo_client::LampoClient;
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

#[macro_export]
macro_rules! async_wait {
    ($callback:tt, $timeout:expr) => {
        async {
            let mut success = false;
            for _ in 0..4 {
                let result = $callback.await;
                if let Err(_) = result {
                    std::thread::sleep(std::time::Duration::from_secs($timeout));
                    continue;
                }
                success = true;
                break;
            }
            assert!(success, "callback got a timeout");
        }
        .await
    };
    ($callback:expr) => {
        $crate::async_wait!($callback, 5);
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
    pub ws_url: String,
}

impl LampoTesting {
    pub async fn new(btc: Arc<BtcNode>) -> error::Result<Self> {
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
        // SAFETY: this should be safe because if the system has no
        // ports it is a bug
        let port = port::random_free_port().unwrap();

        let ws_url = format!("127.0.0.1:{port}");
        log::info!("ws url: `{ws_url}`");
        // FIXME: This can be an InMemory Handler without any problem
        let mut server = JSONRPCv2::new(lampo.clone(), &ws_url)?;
        server.add_rpc("getinfo", get_info).unwrap();
        server.add_rpc("connect", json_connect).unwrap();
        server.add_rpc("fundchannel", json_open_channel).unwrap();
        server.add_rpc("newaddr", json_new_addr).unwrap();
        server.add_rpc("channels", json_list_channels).unwrap();
        server.add_rpc("funds", json_funds).unwrap();
        server.add_rpc("invoice", json_invoice).unwrap();
        server.add_rpc("offer", json_offer).unwrap();
        server.add_rpc("decode", json_decode_invoice).unwrap();
        server.add_rpc("pay", json_pay).unwrap();
        server.add_rpc("keysend", json_keysend).unwrap();
        server.add_rpc("close", json_close_channel).unwrap();
        server.listen().await?;

        let client = LampoClient::new(&format!("ws://{ws_url}")).await?;
        lampo.add_external_handler(Arc::new(client))?;

        // run lampo and take the handler over to run commands
        let handler = lampo.handler();
        lampo.listen().await?;

        // wait that lampo starts
        std::thread::sleep(Duration::from_secs(1));

        let info: response::GetInfo = handler.call("getinfo", json::json!({})).await?;
        log::info!("ready for integration testing `{:?}`!", info);
        Ok(Self {
            inner: handler,
            mnemonic,
            port: port.into(),
            wallet,
            btc,
            root_path: Arc::new(dir),
            info,
            ws_url,
        })
    }

    pub async fn fund_wallet(
        &self,
        blocks: u64,
    ) -> error::Result<bitcoincore_rpc::bitcoin::Address> {
        use clightning_testing::prelude::bitcoincore_rpc::RpcApi;

        // mine some bitcoin inside the lampo address
        let address: NewAddress = self
            .lampod()
            .call("newaddr", json::json!({}))
            .await
            .unwrap();
        let address = bitcoincore_rpc::bitcoin::Address::from_str(&address.address)
            .unwrap()
            .assume_checked();
        let _ = self
            .btc
            .rpc()
            .generate_to_address(blocks, &address)
            .unwrap();

        async_wait!(async {
            let funds: response::Utxos = self.inner.call("funds", json::json!({})).await.unwrap();
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
