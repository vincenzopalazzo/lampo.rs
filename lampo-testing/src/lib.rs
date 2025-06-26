//! Lampo test framework.
pub mod prelude {
    pub use clightning_testing::prelude::btc::Node as BtcNode;
    pub use clightning_testing::prelude::*;
    pub use clightning_testing::*;
    pub use lampod;
    pub use lampod::async_run;
}

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use clightning_testing::prelude::btc::Conf;
use clightning_testing::prelude::btc::Node as BtcNode;
use clightning_testing::prelude::*;
use tempfile::TempDir;

use lampo_bdk_wallet::BDKWalletManager;
use lampo_chain::LampoChainSync;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json;
use lampo_common::model::request;
use lampo_common::model::response;
use lampo_httpd::handler::HttpdHandler;
use lampod::actions::handler::LampoHandler;
use lampod::chain::WalletManager;
use lampod::LampoDaemon;

#[macro_export]
macro_rules! async_wait {
    ($callback:expr, $timeout:expr) => {{
        let mut success = false;
        let max_retries = 10; // Increased from 4 to 10 for more robust testing
        for attempt in 0..max_retries {
            log::debug!(target: "async_wait", "Attempt {}/{} with timeout {}s", attempt + 1, max_retries, $timeout);
            let result = $callback.await;
            if let Err(_) = result {
                // Add some logging for debugging
                log::debug!(target: "async_wait", "Attempt {}/{} failed, retrying in {}s", attempt + 1, max_retries, $timeout);
                tokio::time::sleep(std::time::Duration::from_secs($timeout)).await;
                continue;
            }
            success = true;
            break;
        }
        assert!(success, "async_wait callback got a timeout after {} attempts with {}s intervals", max_retries, $timeout);
    }};
    ($callback:expr) => {
        $crate::async_wait!($callback, 5);
    };
}

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

// Write a macros that will be invoked like `mine_to!("address", 100)` and will
macro_rules! mine {
    ($blocks:expr) => {
        // mine some bitcoin inside the lampo address
        let address = self.wallet.get_onchain_address().await?;
        let address = bitcoincore_rpc::bitcoin::Address::from_str(&address.address)
            .unwrap()
            .assume_checked();
        let _ = rpc.generate_to_address($blocks, &address).unwrap();
        self.wallet.sync().await.unwrap();
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
    pub async fn tmp() -> error::Result<Self> {
        let mut conf = Conf::default();
        conf.wallet = None;
        let conf = Arc::new(conf);
        Self::with_conf(conf).await
    }

    pub async fn with_conf(conf: Arc<Conf<'static>>) -> error::Result<Self> {
        let conf_clone = conf.clone();
        let btc = tokio::task::spawn_blocking(move || {
            if let Ok(exec_path) = btc::exe_path() {
                let btc = BtcNode::with_conf(exec_path, conf_clone.as_ref())?;
                Ok(btc)
            } else {
                anyhow::bail!("corepc-node exec path not found");
            }
        })
        .await??;
        let btc = Arc::new(btc);
        Self::new(btc).await
    }

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
        lampo_conf.api_port = port::random_free_port().unwrap().into();
        log::info!("listening on port `{}`", lampo_conf.api_port);
        let core_url = btc.rpc_url();

        let values = btc.params.get_cookie_values().unwrap();
        lampo_conf.core_url = Some(core_url);
        lampo_conf.core_user = values.as_ref().and_then(|v| Some(v.user.to_owned()));
        lampo_conf.core_pass = values.and_then(|v| Some(v.password));
        lampo_conf.dev_sync = Some(true);

        lampo_conf
            .ldk_conf
            .channel_handshake_limits
            .force_announced_channel_preference = false;
        log::info!("creating bitcoin core wallet");

        let lampo_conf = Arc::new(lampo_conf);
        let (wallet, mnemonic) = BDKWalletManager::new(lampo_conf.clone()).await?;
        let wallet = Arc::new(wallet);
        wallet.clone().listen().await?;

        let mut lampo = LampoDaemon::new(lampo_conf.clone(), wallet.clone());
        let node = Arc::new(LampoChainSync::new(lampo_conf.clone())?);
        lampo.init(node.clone()).await?;
        log::info!("bitcoin core added inside lampo");

        // run httpd and create the handler that will connect to it
        let handler = Arc::new(HttpdHandler::new(format!(
            "http://{}:{}",
            lampo_conf.api_host, lampo_conf.api_port
        ))?);
        lampo.add_external_handler(handler.clone()).await?;
        log::info!("Handler added to lampo");
        let lampo = Arc::new(lampo);
        run_httpd(lampo.clone()).await?;
        log::info!("httpd started");

        // run lampo and take the handler over to run commands
        let handler = lampo.handler();
        tokio::spawn(lampo.listen());

        // wait that lampo starts
        while let Err(err) = handler
            .call::<json::Value, response::GetInfo>("getinfo", json::json!({}))
            .await
        {
            log::error!("error: `{}`", err);
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }

        let info: response::GetInfo = handler.call("getinfo", json::json!({})).await?;
        log::info!("ready `{:#?}` for integration testing!", info);
        let node = Self {
            inner: handler,
            mnemonic,
            port: port.into(),
            wallet,
            btc,
            root_path: Arc::new(dir),
            info,
        };
        node.fund_wallet(102).await?;
        Ok(node)
    }

    async fn mine(&self, blocks: u64) -> error::Result<()> {
        let addr = self.wallet.get_onchain_address().await?;
        let addr = lampo_common::bitcoin::Address::from_str(&addr.address)
            .unwrap()
            .assume_checked();
        // mine some bitcoin inside the lampo address
        let _ = self
            .btc
            .client
            .generate_to_address(blocks as usize, &addr)
            .unwrap();
        self.wallet.sync().await?;
        Ok(())
    }

    pub async fn fund_wallet(&self, blocks: u64) -> error::Result<()> {
        let rpc = self.btc.clone();

        self.mine(blocks).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;

        let wallet = self.wallet.clone();
        async_wait!(async {
            log::info!("waiting for funds to be available");
            let funds: response::Utxos = self.inner.call("funds", json::json!({})).await.unwrap();
            if funds.transactions.is_empty() {
                return Err(());
            }

            let tip = wallet.wallet_tips().await.unwrap();
            // FIXME: we do not need to fail if there is an error in this RPC call
            // but some json error will happen so, lets skip it if we have an error.
            let bitcoind_tip = rpc.client.get_blockchain_info();
            if let Ok(bitcoind_tip) = bitcoind_tip {
                log::info!("bitcoind tip: {:?}", bitcoind_tip);

                if tip.to_consensus_u32() as i64 != bitcoind_tip.blocks {
                    log::warn!(
                        "tip mismatch: wallet tip `{}` and bitcoind tip `{}`",
                        tip,
                        bitcoind_tip.blocks
                    );
                    self.mine(1).await.unwrap();
                    return Err(());
                }
            }
            if wallet.get_onchain_balance().await.unwrap() == 0 {
                self.mine(1).await.unwrap();
                return Err(());
            }

            Ok(())
        });
        Ok(())
    }

    pub async fn fund_channel_with(
        &self,
        counterparty: Arc<LampoTesting>,
        amount: u64,
    ) -> error::Result<()> {
        let _: response::Connect = self
            .lampod()
            .call(
                "connect",
                request::Connect {
                    node_id: counterparty.info.node_id.clone(),
                    addr: "127.0.0.1".to_owned(),
                    port: counterparty.port,
                },
            )
            .await
            .unwrap();

        let mut events = counterparty.lampod().events();

        let response: json::Value = self
            .lampod()
            .call(
                "fundchannel",
                request::OpenChannel {
                    node_id: counterparty.info.node_id.clone(),
                    amount: 100000,
                    public: true,
                    port: None,
                    addr: None,
                },
            )
            .await
            .unwrap();
        assert!(response.get("tx").is_some(), "{:?}", response);
        self.fund_wallet(10).await.unwrap();

        async_wait!(async {
            while let Some(event) = events.recv().await {
                log::info!(target: "tests", "Event received {:?}", event);
                if let Event::Lightning(LightningEvent::ChannelReady {
                    counterparty_node_id,
                    ..
                }) = event
                {
                    if counterparty_node_id.to_string() != self.info.node_id.to_string() {
                        return Err(());
                    }
                    return Ok(());
                };
                // check if lampo see the channel
                let channels: response::Channels = counterparty
                    .lampod()
                    .call("channels", json::json!({}))
                    .await
                    .unwrap();
                log::info!(target: "tests", "Channels {:?}", channels);
                if channels.channels.is_empty() {
                    return Err(());
                }

                if channels.channels.first().unwrap().ready {
                    return Ok(());
                }
            }
            Err(())
        });
        Ok(())
    }

    pub fn lampod(&self) -> Arc<LampoHandler> {
        self.inner.clone()
    }

    pub fn root_path(&self) -> Arc<TempDir> {
        self.root_path.clone()
    }
}
