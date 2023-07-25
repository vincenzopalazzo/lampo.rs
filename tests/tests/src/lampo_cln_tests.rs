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
pub fn fund_a_simple_channel() {
    init();

    let mut cln = async_run!(cln::Node::with_params(
        "--dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
        "regtest"
    ))
    .unwrap();
    let btc = cln.btc();
    let lampo_manager = LampoTesting::new(&btc).unwrap();
    let lampo = lampo_manager.lampod();
    let info = cln.rpc().getinfo().unwrap().clone();
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
    let _: json::Value = lampo
        .call(
            "fundchannel",
            request::OpenChannel {
                node_id: info.id.clone().to_string(),
                port: Some(cln.port.into()),
                amount: 100000,
                public: true,
                addr: Some("127.0.0.1".to_owned()),
            },
        )
        .unwrap();

    // mine some blocks
    let _ = btc.rpc().generate_to_address(6, &address).unwrap();
    wait!(|| {
        let event = events.recv_timeout(Duration::from_secs(1));
        let Ok(Event::Lightning(LightningEvent::ChannelReady { .. })) = event else {
            log::info!("event received {:?}", event);
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
