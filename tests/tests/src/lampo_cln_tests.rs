use std::str::FromStr;
use std::time::Duration;

use lampo_common::error;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json;
use lampo_common::model::request;
use lampo_common::model::response;
use lampo_common::model::response::NewAddress;
use lampo_common::model::Connect;
use lampo_testing::prelude::bitcoincore_rpc::RpcApi;
use lampo_testing::prelude::*;
use lampo_testing::wait;
use lampo_testing::LampoTesting;

use crate::init;

#[test]
pub fn init_connection_test() -> error::Result<()> {
    init();
    let cln = async_run!(cln::Node::tmp("regtest"))?;
    let lampo = LampoTesting::new(cln.btc())?;
    let info = cln.rpc().getinfo()?;
    log::debug!("core lightning info {:?}", info);
    let response: json::Value = lampo.lampod().call(
        "connect",
        Connect {
            node_id: info.id,
            addr: "127.0.0.1".to_owned(),
            port: cln.port.into(),
        },
    )?;
    log::debug!("lampo connected with cln {:?}", response);
    Ok(())
}

#[test]
pub fn fund_a_simple_channel_from_lampo() {
    init();

    let mut cln = async_run!(cln::Node::with_params(
        "--dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
        "regtest"
    ))
    .unwrap();
    let btc = cln.btc();
    let lampo_manager = LampoTesting::new(btc).unwrap();
    let lampo = lampo_manager.lampod();
    let info = cln.rpc().getinfo().unwrap();
    let response: json::Value = lampo
        .call(
            "connect",
            Connect {
                node_id: info.id.clone(),
                addr: "127.0.0.1".to_owned(),
                port: cln.port.into(),
            },
        )
        .unwrap();
    log::debug!("lampo connected with cln {:?}", response);
    // mine some bitcoin inside the lampo address
    let address: NewAddress = lampo.call("newaddr", json::json!({})).unwrap();
    let address = bitcoincore_rpc::bitcoin::Address::from_str(&address.address)
        .unwrap()
        .assume_checked();
    let result = btc.rpc().generate_to_address(6, &address).unwrap();
    log::info!("generate to the addr `{:?}`: `{:?}`", address, result);
    wait!(|| {
        let result: response::Utxos = lampo.call("funds", json::json!({})).unwrap();
        if !result.transactions.is_empty() {
            log::info!(target: "cln-test", "transactiosn {:?}", result);
            return Ok(());
        }
        let _ = btc.rpc().generate_to_address(6, &address).unwrap();
        Err(())
    });

    log::info!("core lightning info {:?}", cln.rpc().getinfo());
    let events = lampo.events();
    let balance = lampo_manager.wallet.get_onchain_balance().unwrap();
    log::info!("lampo wallet balance: {balance}");
    log::info!("funding started");
    let _: json::Value = lampo
        .call(
            "fundchannel",
            request::OpenChannel {
                node_id: info.id,
                port: Some(cln.port.into()),
                amount: 100000,
                public: true,
                addr: Some("127.0.0.1".to_owned()),
            },
        )
        .unwrap();
    log::info!("funding ended");
    // mine some blocks
    let _ = btc.rpc().generate_to_address(6, &address).unwrap();
    wait!(|| {
        let event = events.recv();
        log::trace!("{:?}", event);
        let Ok(Event::Lightning(LightningEvent::ChannelReady { .. })) = event else {
            log::warn!("event received {:?}", event);
            let _ = btc.rpc().generate_to_address(1, &address).unwrap();
            return Err(());
        };
        Ok(())
    });

    let channels = cln.rpc().listfunds().unwrap().channels;
    wait!(|| {
        if channels.is_empty() {
            return Err(());
        }
        Ok(())
    });
    async_run!(cln.stop()).unwrap();
}

#[test]
pub fn fund_a_simple_channel_to_lampo() {
    init();

    let mut cln = async_run!(cln::Node::with_params(
        "--dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
        "regtest"
    ))
    .unwrap();
    let btc = cln.btc();
    let lampo_manager = LampoTesting::new(btc).unwrap();
    let lampo = lampo_manager.lampod();
    let info: response::GetInfo = lampo.call("getinfo", json::json!({})).unwrap();
    let response = cln
        .rpc()
        .connect(
            &info.node_id,
            Some(&format!("127.0.0.1:{}", lampo_manager.port)),
        )
        .unwrap();
    log::debug!("cln connected with cln {:?}", response);
    // mine some bitcoin inside the lampo address
    let address = cln.rpc().newaddr(None).unwrap();
    let address = bitcoincore_rpc::bitcoin::Address::from_str(&address.bech32.unwrap())
        .unwrap()
        .assume_checked();
    let result = btc.rpc().generate_to_address(6, &address).unwrap();
    log::info!("generate to the addr `{:?}`: `{:?}`", address, result);
    wait!(|| {
        let cln_info = cln.rpc().getinfo();
        log::info!("cln info: {:?}", cln_info);
        if cln_info.unwrap().warning_lightningd_sync.is_some() {
            return Err(());
        }
        let mut result = cln.rpc().listfunds().unwrap().outputs;
        result.retain(|tx| tx.status == "confirmed");
        log::info!(target: "cln-test", "confirmed transactions {:?}", result);
        if !result.is_empty() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
        let _ = btc.rpc().generate_to_address(6, &address).unwrap();
        Err(())
    });

    // looks like that rust is too fast and cln is not able to
    // index all the tx, so this will accept some errors
    wait!(|| {
        let result = cln.rpc().fundchannel(
            &info.node_id,
            clightningrpc::requests::AmountOrAll::Amount(3000000),
            None,
        );
        if result.is_err() {
            let message = result.err().unwrap().to_string();
            log::error!("{message}");
            if !message.contains("afford") {
                return Ok(());
            }
            let address: NewAddress = lampo.call("newaddr", json::json!({})).unwrap();
            let address = bitcoincore_rpc::bitcoin::Address::from_str(&address.address)
                .unwrap()
                .assume_checked();
            let _ = btc.rpc().generate_to_address(1, &address).unwrap();
            std::thread::sleep(Duration::from_secs(2));
            return Err(());
        }
        Ok(())
    });

    // mine some blocks
    let _ = btc.rpc().generate_to_address(6, &address).unwrap();
    wait!(|| {
        let channels = cln.rpc().listfunds().unwrap().channels;
        if channels.is_empty() {
            return Err(());
        }
        Ok(())
    });
    async_run!(cln.stop()).unwrap();
}
