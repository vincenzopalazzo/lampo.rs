use std::str::FromStr;
use std::time::Duration;

use lampo_common::error;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json;
use lampo_common::model::request;
use lampo_common::model::response;
use lampo_common::model::response::InvoiceInfo;
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
    // mine some blocks
    let _ = btc.rpc().generate_to_address(6, &address).unwrap();
    wait!(|| {
        while let Ok(event) = events.recv_timeout(Duration::from_millis(100)) {
            log::trace!("{:?}", event);
            let Event::Lightning(LightningEvent::ChannelReady { .. }) = event else {
                log::warn!("event received {:?}", event);
                let _ = btc.rpc().generate_to_address(1, &address).unwrap();
                continue;
            };
            return Ok(());
        }
        Err(())
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

#[test]
pub fn payinvoice_to_lampo() {
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
        if channels.first().unwrap().state != "CHANNELD_NORMAL".to_string() {
            return Err(());
        }
        Ok(())
    });
    let funds: json::Value = cln.rpc().call("listfunds", json::json!({})).unwrap();
    log::info!("list funds core lightning: {:?}", funds);
    let invoice: response::Invoice = lampo
        .call(
            "invoice",
            json::json!({
                "amount_msat": 1000,
                "description": "integration_test",
            }),
        )
        .unwrap();
    let _: json::Value = cln
        .rpc()
        .call(
            "pay",
            json::json!({
                "bolt11": invoice.bolt11,
            }),
        )
        .unwrap();
    async_run!(cln.stop()).unwrap();
}

#[test]
pub fn decode_invoice_from_cln() {
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
    let invoce = cln
        .rpc()
        .invoice(
            None,
            "lampo",
            "need to be decoded by lampo",
            None,
            None,
            None,
        )
        .unwrap();
    let decode: InvoiceInfo = lampo
        .call(
            "decode_invoice",
            json::json!({
                "invoice_str": invoce.bolt11,
            }),
        )
        .unwrap();
    assert_eq!(decode.description, "need to be decoded by lampo");
    async_run!(cln.stop()).unwrap();
}

#[test]
pub fn no_able_to_pay_invoice_to_cln() {
    init();

    let mut cln = async_run!(cln::Node::with_params(
        "--dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
        "regtest"
    ))
    .unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let btc = cln.btc();
    let lampo_manager = LampoTesting::new(btc).unwrap();
    let lampo = lampo_manager.lampod();

    let invoce = cln
        .rpc()
        .invoice(
            Some(1000),
            "lampo",
            "need to be decoded by lampo",
            None,
            None,
            None,
        )
        .unwrap();
    let result: error::Result<json::Value> = lampo.call(
        "pay",
        json::json!({
            "invoice_str": invoce.bolt11,
        }),
    );
    // there is no channel so we must fails
    assert!(result.is_err());
    async_run!(cln.stop()).unwrap();
}

#[test]
pub fn pay_invoice_to_cln() {
    init();

    let mut cln = async_run!(cln::Node::with_params(
        "--dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
        "regtest"
    ))
    .unwrap();
    let btc = cln.btc();
    let lampo_manager = LampoTesting::new(btc).unwrap();
    let lampo = lampo_manager.lampod();
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
    let _: json::Value = lampo
        .call(
            "fundchannel",
            request::OpenChannel {
                node_id: cln.rpc().getinfo().unwrap().id,
                port: Some(cln.port.into()),
                amount: 100000000,
                public: true,
                addr: Some("127.0.0.1".to_owned()),
            },
        )
        .unwrap();
    // mine some blocks
    let _ = btc.rpc().generate_to_address(6, &address).unwrap();
    wait!(|| {
        while let Ok(event) = events.recv_timeout(Duration::from_millis(100)) {
            log::trace!("{:?}", event);
            let Event::Lightning(LightningEvent::ChannelReady { .. }) = event else {
                log::warn!("event received {:?}", event);
                let _ = btc.rpc().generate_to_address(1, &address).unwrap();
                continue;
            };
            return Ok(());
        }
        Err(())
    });

    wait!(|| {
        let channels = cln.rpc().listfunds().unwrap().channels;
        if channels.is_empty() {
            return Err(());
        }
        if channels.first().unwrap().state != "CHANNELD_NORMAL".to_string() {
            return Err(());
        }
        Ok(())
    });

    let invoce = cln
        .rpc()
        .invoice(
            Some(1000),
            "lampo",
            "need to be decoded by lampo",
            None,
            None,
            None,
        )
        .unwrap();
    let result: error::Result<json::Value> = lampo.call(
        "pay",
        json::json!({
            "invoice_str": invoce.bolt11,
        }),
    );
    // there is no channel so we must fails
    assert!(result.is_ok(), "{:?}", result);
    async_run!(cln.stop()).unwrap();
}
