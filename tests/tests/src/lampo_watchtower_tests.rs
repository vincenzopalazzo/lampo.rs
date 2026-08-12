//! Watchtower (TEOS) integration tests.
//!
//! The capture test needs no tower: it points lampo at an unreachable
//! URL and checks that justice transactions pile up in the durable
//! outbox while the node keeps operating.
//!
//! The breach test drives a real `teosd` (skipped when the binary is
//! not installed): lampo backs up every state to the tower, then the
//! CLN counterparty broadcasts a revoked commitment — captured earlier
//! with `dev-sign-last-tx` — and the test asserts the tower responds
//! to the breach.

use std::str::FromStr;
use std::time::Duration;

use lampo_common::bitcoin::consensus;
use lampo_common::bitcoin::Transaction;
use lampo_common::error;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json;
use lampo_common::model::request;
use lampo_common::model::response;
use lampo_common::secp256k1::PublicKey;
use lampo_testing::async_wait;
use lampo_testing::prelude::*;
use lampo_testing::LampoTesting;

use crate::init;

/// Opens a channel from lampo to cln and waits until it is usable.
async fn open_channel(lampo_manager: &LampoTesting, cln: &cln::Node) -> error::Result<()> {
    let lampo = lampo_manager.lampod();
    let info = cln.rpc().getinfo()?;
    let mut events = lampo.events();
    let response: json::Value = lampo
        .call(
            "fundchannel",
            request::OpenChannel {
                node_id: info.id,
                port: Some(cln.port.into()),
                amount: 500_000_000,
                public: true,
                addr: Some("127.0.0.1".to_owned()),
            },
        )
        .await?;
    assert!(response.get("tx").is_some(), "{:?}", response);

    lampo_manager.fund_wallet(3).await?;
    async_wait!(async {
        while let Some(event) = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .map_err(|_| ())?
        {
            let Event::Lightning(LightningEvent::ChannelReady { .. }) = event else {
                lampo_manager.fund_wallet(3).await.unwrap();
                return Err(());
            };
            // check that also cln sees the channel as usable
            let mut channels = cln.rpc().listfunds().unwrap().channels;
            let origin_size = channels.len();
            channels.retain(|chan| chan.state == "CHANNELD_NORMAL");
            if !channels.is_empty() && channels.len() == origin_size {
                return Ok(());
            }
            lampo_manager.fund_wallet(1).await.unwrap();
            return Err(());
        }
        Err(())
    });
    Ok(())
}

/// Pays a cln invoice from lampo.
async fn pay_invoice(
    lampo_manager: &LampoTesting,
    cln: &cln::Node,
    label: &str,
    msat: u64,
) -> error::Result<()> {
    let invoice = cln.rpc().invoice(
        Some(msat),
        label,
        "watchtower test invoice",
        None,
        None,
        None,
    )?;
    let result: json::Value = lampo_manager
        .lampod()
        .call("pay", json::json!({ "invoice_str": invoice.bolt11 }))
        .await?;
    log::info!(target: "tests", "payment result: {result}");
    Ok(())
}

// Like the other CLN payment tests, this wedges the 2-core CI runner
// when scheduled next to another CLN+bitcoind test. Run it manually
// with `cargo test -p tests -- --ignored`.
#[ignore = "CLN payment tests starve the CI runner, run manually"]
#[tokio_test_shutdown_timeout::test(1)]
pub async fn watchtower_captures_justice_txs_while_tower_unreachable() -> error::Result<()> {
    init();

    let cln = cln::Node::with_params(
        "--developer --dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
    )
    .await?;
    let btc = cln.btc();
    // Nothing listens on this port: the tower is configured but
    // unreachable, channel operation must not be affected.
    let dead_port = port::random_free_port().unwrap();
    let lampo_manager = LampoTesting::new_with_conf(btc.clone(), |conf| {
        conf.watchtower_url = Some(format!("http://127.0.0.1:{dead_port}"));
        // Any valid public key: no receipt will ever be verified.
        conf.watchtower_id =
            Some("0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798".to_owned());
    })
    .await?;

    open_channel(&lampo_manager, &cln).await?;
    // Two payments: the first gives cln a balance worth punishing, the
    // second revokes that state, which makes its justice tx signable.
    pay_invoice(&lampo_manager, &cln, "wt-capture-1", 100_000_000).await?;
    pay_invoice(&lampo_manager, &cln, "wt-capture-2", 100_000_000).await?;

    // The revoked commitment's signed justice tx must reach the
    // outbox, and stay there since the tower is unreachable.
    let outbox = lampo_manager
        .root_path()
        .path()
        .join("regtest/watchtower/outbox");
    async_wait!(async {
        let entries = std::fs::read_dir(&outbox)
            .map(|dir| dir.count())
            .unwrap_or(0);
        log::info!(target: "tests", "outbox entries: {entries}");
        if entries > 0 {
            return Ok(());
        }
        Err(())
    });

    // The node is still operational with the tower down.
    let _: response::GetInfo = lampo_manager
        .lampod()
        .call("getinfo", json::json!({}))
        .await?;
    Ok(())
}

/// A `teosd` process bound to the test bitcoind, killed on drop.
struct TowerProcess {
    child: std::process::Child,
    pub api_port: u16,
    pub tower_id: String,
    _datadir: tempfile::TempDir,
}

impl Drop for TowerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl TowerProcess {
    fn spawn(btc: &BtcNode) -> error::Result<Self> {
        let datadir = tempfile::tempdir()?;
        let api_port = port::random_free_port().unwrap();
        let rpc_port = port::random_free_port().unwrap();
        let cookie = btc
            .params
            .get_cookie_values()?
            .ok_or(error::anyhow!("bitcoind cookie not found"))?;
        // The default chain polling delta is 60s: way too slow for a
        // test. There is no CLI flag for it, only the config file.
        std::fs::write(datadir.path().join("teos.toml"), "polling_delta = 1\n")?;
        let stdout = std::fs::File::create(datadir.path().join("teosd.log"))?;
        let stderr = std::fs::File::create(datadir.path().join("teosd.err"))?;
        let child = std::process::Command::new("teosd")
            .args([
                "--datadir",
                datadir.path().to_str().unwrap(),
                "--btcnetwork",
                "regtest",
                "--btcrpcconnect",
                "127.0.0.1",
                "--btcrpcport",
                &btc.params.rpc_socket.port().to_string(),
                "--btcrpcuser",
                &cookie.user,
                "--btcrpcpassword",
                &cookie.password,
                "--apibind",
                "127.0.0.1",
                "--apiport",
                &api_port.to_string(),
                "--rpcbind",
                "127.0.0.1",
                "--rpcport",
                &rpc_port.to_string(),
            ])
            .stdout(stdout)
            .stderr(stderr)
            .spawn()?;

        let mut tower = TowerProcess {
            child,
            api_port,
            tower_id: String::new(),
            _datadir: datadir,
        };

        // Wait for the tower to come up and learn its tower id.
        let mut last_err = String::new();
        for _ in 0..30 {
            std::thread::sleep(Duration::from_secs(1));
            let info = std::process::Command::new("teos-cli")
                .args([
                    "--datadir",
                    tower._datadir.path().to_str().unwrap(),
                    "--rpcbind",
                    "127.0.0.1",
                    "--rpcport",
                    &rpc_port.to_string(),
                    "gettowerinfo",
                ])
                .output()?;
            if info.status.success() {
                let info: json::Value = json::from_slice(&info.stdout)?;
                tower.tower_id = info["tower_id"]
                    .as_str()
                    .ok_or(error::anyhow!("no tower_id in gettowerinfo"))?
                    .to_owned();
                return Ok(tower);
            }
            last_err = String::from_utf8_lossy(&info.stderr).to_string();
        }
        let teosd_log =
            std::fs::read_to_string(tower._datadir.path().join("teosd.log")).unwrap_or_default();
        let teosd_err =
            std::fs::read_to_string(tower._datadir.path().join("teosd.err")).unwrap_or_default();
        error::bail!(
            "teosd did not come up: {last_err}\nteosd stdout:\n{teosd_log}\nteosd stderr:\n{teosd_err}"
        )
    }
}

fn teosd_available() -> bool {
    std::process::Command::new("teosd")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[tokio_test_shutdown_timeout::test(1)]
pub async fn watchtower_responds_to_breach_with_teosd() -> error::Result<()> {
    init();
    if !teosd_available() {
        eprintln!("teosd not found in PATH, skipping the breach test");
        return Ok(());
    }

    let cln = cln::Node::with_params(
        "--developer --dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
    )
    .await?;
    let btc = cln.btc();
    // teosd refuses to start on a chain shorter than 100 blocks.
    let mine_addr = cln.rpc().newaddr(None)?.bech32.unwrap();
    crate::utils::fund_wallet(btc.clone(), &mine_addr, 101)?;
    let tower = tokio::task::block_in_place(|| TowerProcess::spawn(&btc))?;
    log::info!(target: "tests", "teosd up: tower_id {}", tower.tower_id);

    let tower_url = format!("http://localhost:{}", tower.api_port);
    let tower_id = tower.tower_id.clone();
    let lampo_manager = LampoTesting::new_with_conf(btc.clone(), move |conf| {
        conf.watchtower_url = Some(tower_url);
        conf.watchtower_id = Some(tower_id);
    })
    .await?;

    open_channel(&lampo_manager, &cln).await?;

    // State A: cln holds 100k sat. Capture cln's current commitment
    // before it gets revoked.
    pay_invoice(&lampo_manager, &cln, "wt-breach-1", 100_000_000).await?;
    let lampo_node_id = lampo_manager.info.node_id.clone();
    let revoked: json::Value = cln
        .rpc()
        .call("dev-sign-last-tx", json::json!({ "id": lampo_node_id }))?;
    let revoked_tx = revoked["tx"]
        .as_str()
        .ok_or(error::anyhow!("no tx in dev-sign-last-tx: {revoked}"))?
        .to_owned();
    let breach_txid =
        consensus::deserialize::<Transaction>(&lampo_common::hex::decode(&revoked_tx)?)?
            .compute_txid();
    log::info!(target: "tests", "captured commitment {breach_txid} to breach later");

    // State B: revoke state A with another payment.
    pay_invoice(&lampo_manager, &cln, "wt-breach-2", 100_000_000).await?;

    // The appointment for the (about to be) breached state must reach
    // the tower.
    let user_sk_path = lampo_manager
        .root_path()
        .path()
        .join("regtest/watchtower/user_sk");
    let tower_client = || -> error::Result<lampo_watchtower::client::TowerClient> {
        let user_sk = lampo_common::secp256k1::SecretKey::from_str(
            std::fs::read_to_string(&user_sk_path)?.trim(),
        )?;
        Ok(lampo_watchtower::client::TowerClient::new(
            format!("http://localhost:{}", tower.api_port),
            PublicKey::from_str(&tower.tower_id)?,
            user_sk,
        ))
    };
    async_wait!(async {
        let Ok(client) = tower_client() else {
            return Err(());
        };
        let Ok(appointment) = client.get_appointment(&breach_txid).await else {
            return Err(());
        };
        log::info!(target: "tests", "appointment on tower: {appointment}");
        if appointment["status"] == "being_watched" {
            return Ok(());
        }
        Err(())
    });

    // Breach: broadcast the revoked commitment and confirm it.
    let _: json::Value = btc
        .client
        .call("sendrawtransaction", &[json::json!(revoked_tx)])
        .map_err(|err| error::anyhow!("broadcasting the revoked commitment: {err}"))?;

    // The tower must detect the breach in a block and respond with the
    // penalty transaction.
    async_wait!(async {
        let _ = crate::utils::fund_wallet(btc.clone(), &mine_addr, 1);
        let Ok(client) = tower_client() else {
            return Err(());
        };
        let Ok(appointment) = client.get_appointment(&breach_txid).await else {
            return Err(());
        };
        log::info!(target: "tests", "appointment on tower after breach: {appointment}");
        if appointment["status"] == "dispute_responded" {
            return Ok(());
        }
        Err(())
    });
    Ok(())
}
