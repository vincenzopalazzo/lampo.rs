//! Lampo test framework.
pub mod prelude {
    pub use clightning_testing::prelude::*;
    pub use clightning_testing::*;
    pub use lampod;
    pub use lampod::async_run;
}

use std::str::FromStr;
use std::sync::Arc;

use clightning_testing::btc::BtcNode;
use clightning_testing::prelude::*;
use lampo_httpd::handler::HttpdHandler;
use tempfile::TempDir;

use lampo_bitcoind::BitcoinCore;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::json;
use lampo_common::model::response;
use lampo_core_wallet::CoreWalletManager;
use lampod::actions::handler::LampoHandler;
use lampod::chain::WalletManager;
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

pub async fn run_httpd(lampod: Arc<LampoDaemon>) -> error::Result<()> {
    let url = format!("{}:{}", lampod.conf().api_host, lampod.conf().api_port);
    let mut http_hosting = url.clone();
    if let Some(clean_url) = url.strip_prefix("http://") {
        http_hosting = clean_url.to_string();
    } else if let Some(clean_url) = url.strip_prefix("https://") {
        http_hosting = clean_url.to_string();
    }
    log::info!("preparing httpd api on addr `{url}`");
    tokio::spawn(lampo_httpd::run(lampod, http_hosting, url));
    Ok(())
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
        lampo_conf.api_port = port::random_free_port().unwrap().into();
        log::info!("listening on port `{}`", lampo_conf.api_port);
        let core_url = format!("127.0.0.1:{}", btc.port);
        lampo_conf.core_pass = Some(btc.pass.clone());
        lampo_conf.core_url = Some(core_url);
        lampo_conf.core_user = Some(btc.user.clone());
        lampo_conf
            .ldk_conf
            .channel_handshake_limits
            .force_announced_channel_preference = false;
        log::info!("creating bitcoin core wallet");
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
        log::info!("bitcoin core added inside lampo");

        // run httpd and create the handler that will connect to it
        let handler = Arc::new(HttpdHandler::new(format!(
            "{}:{}",
            lampo_conf.api_host, lampo_conf.api_port
        ))?);
        lampo.add_external_handler(handler.clone())?;
        log::info!("Handler added to lampo");
        let lampo = Arc::new(lampo);
        tokio::spawn(run_httpd(lampo.clone()));
        log::info!("httpd started");

        // run lampo and take the handler over to run commands
        let handler = lampo.handler();
        std::thread::spawn(move || lampo.listen().unwrap().join());

        // wait that lampo starts
        wait!(|| {
            let info = handler.call::<json::Value, response::GetInfo>("getinfo", json::json!({}));
            log::warn!("info {:?}", info);
            if info.is_err() {
                return Err(());
            }
            Ok(())
        });

        // FIXME: wait that lampo starts
        let info: response::GetInfo = handler.call("getinfo", json::json!({}))?;
        log::info!("ready `{:#?}` for integration testing!", info);
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
        let address: response::NewAddress = self.lampod().call("newaddr", json::json!({})).unwrap();
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
