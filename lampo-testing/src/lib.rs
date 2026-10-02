//! Lampo test framework.
pub mod prelude {
    pub use clightning_testing::prelude::btc::Node as BtcNode;
    pub use clightning_testing::prelude::*;
    pub use clightning_testing::*;
    pub use lampod;
    pub use lampod::async_run;
}

#[cfg(feature = "vls")]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Child;
#[cfg(feature = "vls")]
use std::process::{Command, Stdio};
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
use lampo_common::types::NodeId;
use lampo_httpd::handler::HttpdHandler;
use lampo_lnd::LndRestConfig;
#[cfg(feature = "vls")]
use lampo_vls::{VlsSigner, VlsSignerConfig};
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

pub async fn run_lnd_rest(lampod: Arc<LampoDaemon>, port: u16) -> error::Result<(u16, String)> {
    let data = lampod.conf().path();
    let tls_dir = format!("{data}/lnd-rest");
    let macaroon_dir = format!("{data}/lnd-rest/macaroons");
    let admin_path = format!("{macaroon_dir}/admin.macaroon");
    let conf = LndRestConfig {
        listen_host: "127.0.0.1".to_string(),
        listen_port: port,
        tls_extra_sans: Vec::new(),
        tls_dir: tls_dir.into(),
        macaroon_dir: macaroon_dir.into(),
    };
    lampo_lnd::spawn(lampod, conf)?;

    for _ in 0..100 {
        if std::path::Path::new(&admin_path).exists() {
            let bytes = tokio::fs::read(&admin_path).await?;
            return Ok((port, hex::encode(bytes)));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    error::bail!("timed out waiting for LND REST admin.macaroon at {admin_path}")
}

/// Paths to the VLS binaries a VLS-backed test node needs. Read from
/// `VLSD_EXE` and `REMOTE_HSMD_SOCKET_EXE`.
#[derive(Clone, Debug)]
pub struct VlsBinaries {
    pub vlsd: PathBuf,
    pub proxy: PathBuf,
}

impl VlsBinaries {
    pub fn from_env() -> Option<Self> {
        let vlsd = std::env::var_os("VLSD_EXE")?;
        let proxy = std::env::var_os("REMOTE_HSMD_SOCKET_EXE")?;
        Some(Self {
            vlsd: PathBuf::from(vlsd),
            proxy: PathBuf::from(proxy),
        })
    }
}

/// A spawned `vlsd`, killed when the node that owns it is dropped.
pub struct VlsdProcess(Child);

impl Drop for VlsdProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start `vlsd` for a node and hand back the connected [`VlsSigner`].
///
/// The wallet's keychain xpubs go into the signer allowlist so closes and
/// sweeps to wallet addresses pass policy; payments are auto-approved since
/// lampo does not preapprove invoices yet.
#[cfg(feature = "vls")]
async fn start_vls(
    bins: &VlsBinaries,
    root: &Path,
    conf: &LampoConf,
    wallet: &BDKWalletManager,
) -> error::Result<(Arc<VlsSigner>, VlsdProcess)> {
    let port = port::random_free_port().unwrap();
    let vls_dir = root.join("vls");
    fs::create_dir_all(&vls_dir)?;
    let xpub = wallet
        .account_xpub()
        .ok_or_else(|| error::anyhow!("the bdk wallet has no account xpub to allowlist"))?;
    let allowlist = vls_dir.join("allowlist");
    fs::write(
        &allowlist,
        lampo_vls::wallet_allowlist(&xpub).join("\n") + "\n",
    )?;
    let log = fs::File::create(vls_dir.join("vlsd.log"))?;
    let vlsd = Command::new(&bins.vlsd)
        .args(["--network", "regtest", "--datadir"])
        .arg(&vls_dir)
        .arg("--connect")
        .arg(format!("http://127.0.0.1:{port}"))
        .env("REMOTE_SIGNER_ALLOWLIST", &allowlist)
        .env("VLS_AUTOAPPROVE", "1")
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    log::info!(
        "vlsd started (pid {}), dialing the proxy on {port}",
        vlsd.id()
    );

    let mut vls_conf = conf.clone();
    vls_conf.signer = Some("vls".to_owned());
    vls_conf.vls_proxy_bin = Some(bins.proxy.to_string_lossy().into_owned());
    vls_conf.vls_port = Some(port);
    let config = VlsSignerConfig::from_conf(&vls_conf)?;
    let signer = tokio::task::spawn_blocking(move || VlsSigner::spawn(config)).await??;
    Ok((signer, VlsdProcess(vlsd)))
}

/// Build a daemon whose keys live in a freshly started `vlsd`.
#[cfg(feature = "vls")]
async fn vls_daemon(
    bins: &VlsBinaries,
    root: &Path,
    conf: Arc<LampoConf>,
    wallet: Arc<BDKWalletManager>,
) -> error::Result<(LampoDaemon, VlsdProcess)> {
    let (signer, vlsd) = start_vls(bins, root, &conf, &wallet).await?;
    Ok((LampoDaemon::with_signer(conf, wallet, signer), vlsd))
}

#[cfg(not(feature = "vls"))]
async fn vls_daemon(
    _bins: &VlsBinaries,
    _root: &Path,
    _conf: Arc<LampoConf>,
    _wallet: Arc<BDKWalletManager>,
) -> error::Result<(LampoDaemon, VlsdProcess)> {
    error::bail!("lampo-testing built without the `vls` feature")
}

pub struct LampoTesting {
    inner: Arc<LampoHandler>,
    /// `vlsd` for a VLS-backed node; killed on drop.
    vlsd: Option<VlsdProcess>,
    daemon: Arc<LampoDaemon>,
    root_path: Arc<TempDir>,
    pub port: u64,
    pub wallet: Arc<dyn WalletManager>,
    pub mnemonic: String,
    pub btc: Arc<BtcNode>,
    pub info: response::GetInfo,
    pub lnd_rest_port: u16,
    pub lnd_admin_macaroon_hex: String,
}

impl LampoTesting {
    pub async fn tmp() -> error::Result<Self> {
        Self::tmp_with(|_| {}).await
    }

    /// Like [`Self::tmp`], but the node's keys live in `vlsd`.
    #[cfg(feature = "vls")]
    pub async fn tmp_with_vls(bins: VlsBinaries) -> error::Result<Self> {
        let mut conf = Conf::default();
        conf.wallet = None;
        let conf = Arc::new(conf);
        Self::with_conf_inner(conf, false, Some(bins), |_| {}).await
    }

    /// Like [`Self::new`], but the node's keys live in `vlsd`.
    #[cfg(feature = "vls")]
    pub async fn new_with_vls(btc: Arc<BtcNode>, bins: VlsBinaries) -> error::Result<Self> {
        Self::new_inner(btc, false, Some(bins), |_| {}).await
    }

    /// Like [`Self::tmp`], but `conf_fn` may adjust the [`LampoConf`] before
    /// the daemon is built from it.
    pub async fn tmp_with(conf_fn: impl FnOnce(&mut LampoConf)) -> error::Result<Self> {
        let mut conf = Conf::default();
        conf.wallet = None;
        let conf = Arc::new(conf);
        Self::with_conf_and(conf, conf_fn).await
    }

    /// Same as [`Self::tmp`], but also starts the LND-compatible REST API.
    ///
    /// Prefer this only from LND REST tests: every node otherwise pays the
    /// cost of an extra Actix HTTPS thread that is never torn down.
    pub async fn tmp_with_lnd_rest() -> error::Result<Self> {
        let mut conf = Conf::default();
        conf.wallet = None;
        let conf = Arc::new(conf);
        Self::with_conf_inner(conf, true, None, |_| {}).await
    }

    pub async fn with_conf(conf: Arc<Conf<'static>>) -> error::Result<Self> {
        Self::with_conf_inner(conf, false, None, |_| {}).await
    }

    pub async fn with_conf_and(
        conf: Arc<Conf<'static>>,
        conf_fn: impl FnOnce(&mut LampoConf),
    ) -> error::Result<Self> {
        Self::with_conf_inner(conf, false, None, conf_fn).await
    }

    async fn with_conf_inner(
        conf: Arc<Conf<'static>>,
        enable_lnd_rest: bool,
        vls: Option<VlsBinaries>,
        conf_fn: impl FnOnce(&mut LampoConf),
    ) -> error::Result<Self> {
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
        Self::new_inner(btc, enable_lnd_rest, vls, conf_fn).await
    }

    pub async fn new(btc: Arc<BtcNode>) -> error::Result<Self> {
        Self::new_inner(btc, false, None, |_| {}).await
    }

    /// Like [`Self::new`], but `conf_fn` may adjust the [`LampoConf`] before
    /// the daemon is built from it.
    pub async fn new_with(
        btc: Arc<BtcNode>,
        conf_fn: impl FnOnce(&mut LampoConf),
    ) -> error::Result<Self> {
        Self::new_inner(btc, false, None, conf_fn).await
    }

    async fn new_inner(
        btc: Arc<BtcNode>,
        enable_lnd_rest: bool,
        vls: Option<VlsBinaries>,
        conf_fn: impl FnOnce(&mut LampoConf),
    ) -> error::Result<Self> {
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
        // Integration tests dial `127.0.0.1:<port>`. ldk-node does not bind
        // unless a listening address is configured, and neither do we, so
        // the harness has to say where it listens. `conf_fn` may override.
        lampo_conf.bind_addr = Some("127.0.0.1".to_owned());

        lampo_conf
            .ldk_conf
            .channel_handshake_limits
            .force_announced_channel_preference = false;
        conf_fn(&mut lampo_conf);
        log::info!("creating bitcoin core wallet");

        let lampo_conf = Arc::new(lampo_conf);
        let (wallet, mnemonic) = BDKWalletManager::new(lampo_conf.clone()).await?;
        let wallet = Arc::new(wallet);

        // `LampoDaemon::new` shares the coordinator with the wallet, so the
        // wallet gates its Emitter on listener sync (production startup flow).
        let (mut lampo, vlsd) = match vls {
            Some(bins) => {
                let (daemon, vlsd) =
                    vls_daemon(&bins, dir.path(), lampo_conf.clone(), wallet.clone()).await?;
                (daemon, Some(vlsd))
            }
            None => (LampoDaemon::new(lampo_conf.clone(), wallet.clone()), None),
        };
        wallet.clone().listen().await?;

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

        let (lnd_port, macaroon_hex) = if enable_lnd_rest {
            let lnd_port = port::random_free_port().unwrap();
            let (_, macaroon_hex) = run_lnd_rest(lampo.clone(), lnd_port).await?;
            log::info!("lnd rest started on {lnd_port}");
            (lnd_port, macaroon_hex)
        } else {
            (0, String::new())
        };

        // run lampo and take the handler over to run commands
        let handler = lampo.handler();
        tokio::spawn(lampo.clone().listen());

        // wait that lampo starts (bounded: an infinite wait hangs the whole CI job)
        let mut ready = false;
        for _ in 0..30 {
            match handler
                .call::<json::Value, response::GetInfo>("getinfo", json::json!({}))
                .await
            {
                Ok(_) => {
                    ready = true;
                    break;
                }
                Err(err) => {
                    log::error!("error: `{}`", err);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
        if !ready {
            error::bail!("lampo failed to become ready via getinfo within 30s");
        }

        let info: response::GetInfo = handler.call("getinfo", json::json!({})).await?;
        log::info!("ready `{:#?}` for integration testing!", info);
        let node = Self {
            inner: handler,
            vlsd,
            daemon: lampo,
            mnemonic,
            port: port.into(),
            wallet,
            btc,
            root_path: Arc::new(dir),
            info,
            lnd_rest_port: lnd_port,
            lnd_admin_macaroon_hex: macaroon_hex,
        };
        node.fund_wallet(102).await?;
        Ok(node)
    }

    /// Whether this node's keys live in an external VLS signer.
    pub fn uses_vls(&self) -> bool {
        self.vlsd.is_some()
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

    /// Counterparty fund channel with us
    /// counterparty is the node that will fund the channel with us
    /// counterparty -> self and not self -> counterparty
    pub async fn fund_channel_with(
        &self,
        // FIXME: we should abstract it to a lightning trait
        counterparty: Arc<LampoTesting>,
        amount: u64,
    ) -> error::Result<()> {
        self.fund_channel_with_privacy(counterparty, amount, true)
            .await
    }

    /// Like [`Self::fund_channel_with`], with explicit control over whether
    /// the channel is announced. Async payments tests need unannounced
    /// recipients: a node that appears in the graph (via a public channel
    /// announcement) but has no announced addresses builds self-introduced
    /// blinded paths that non-peers cannot reach.
    pub async fn fund_channel_with_privacy(
        &self,
        counterparty: Arc<LampoTesting>,
        amount: u64,
        public: bool,
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
                    amount: amount,
                    public,
                    port: None,
                    addr: None,
                    push_msat: None,
                    sat_per_vbyte: None,
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
                    let Ok(expected_node_id) = NodeId::from_str(&self.info.node_id) else {
                        return Err(());
                    };
                    if counterparty_node_id != expected_node_id {
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

    pub fn daemon(&self) -> Arc<LampoDaemon> {
        self.daemon.clone()
    }

    pub fn root_path(&self) -> Arc<TempDir> {
        self.root_path.clone()
    }
}
