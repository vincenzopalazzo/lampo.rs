use std::str::FromStr;
use std::time::Duration;

use lampo_common::error;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::onchain::OnChainEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json;
use lampo_common::model::request;
use lampo_common::model::response;
use lampo_common::model::response::InvoiceInfo;
use lampo_common::model::Connect;
use lampo_common::secp256k1::PublicKey;
use lampo_testing::prelude::bitcoincore_rpc::RpcApi;
use lampo_testing::prelude::*;
use lampo_testing::wait;
use lampo_testing::LampoTesting;

use crate::init;
use crate::utils::*;

#[test]
pub fn init_connection_test() -> error::Result<()> {
    init();
    let cln = async_run!(cln::Node::with_params("--developer", "regtest"))?;
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
        "--developer --dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
        "regtest"
    ))
    .unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let btc = cln.btc();
    let lampo_manager = LampoTesting::new(btc.clone()).unwrap();
    let lampo = lampo_manager.lampod();
    let info = cln.rpc().getinfo().unwrap();
    let _: json::Value = lampo
        .call(
            "connect",
            Connect {
                node_id: info.id.clone(),
                addr: "127.0.0.1".to_owned(),
                port: cln.port.into(),
            },
        )
        .unwrap();

    let events = lampo.events();
    let address = lampo_manager.fund_wallet(101).unwrap();
    wait!(|| {
        let Ok(Event::OnChain(OnChainEvent::NewBestBlock((_, height)))) =
            events.recv_timeout(Duration::from_millis(100))
        else {
            return Err(());
        };
        if height.to_consensus_u32() == 101 {
            return Ok(());
        }
        Err(())
    });

    let response: json::Value = lampo
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
    assert!(response.get("tx").is_some());

    wait!(|| {
        while let Ok(event) = events.recv_timeout(Duration::from_millis(100)) {
            log::trace!("{:?}", event);
            let Event::Lightning(LightningEvent::ChannelReady { .. }) = event else {
                log::info!(target: "tests", "event received {:?}", event);
                let _ = btc.rpc().generate_to_address(1, &address).unwrap();
                continue;
            };
            log::info!(target: "tests", "channel ready event received");
            // check if lampo see the channel
            let channels: response::Channels = lampo.call("channels", json::json!({})).unwrap();
            if channels.channels.is_empty() {
                return Err(());
            }

            if !channels.channels.first().unwrap().ready {
                return Err(());
            }

            let mut channels = cln.rpc().listfunds().unwrap().channels;
            let origin_size = channels.len();
            channels.retain(|chan| chan.state == "CHANNELD_NORMAL");
            if channels.len() == origin_size {
                return Ok(());
            }
            let address = cln.rpc().newaddr(None).unwrap();
            fund_wallet(btc.clone(), &address.bech32.unwrap(), 1).unwrap();
            crate::wait_cln_sync!(cln);
            return Err(());
        }
        Err(())
    });

    // looks like that rust is too fast and cln is not able to
    // index all the tx, so this will accept some errors
    async_run!(cln.stop()).unwrap();
}

#[test]
pub fn fund_a_simple_channel_to_lampo() {
    init();

    let mut cln = async_run!(cln::Node::with_params(
        "--developer --dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
        "regtest"
    ))
    .unwrap();
    let btc = cln.btc();
    let lampo_manager = LampoTesting::new(btc.clone()).unwrap();
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
    fund_wallet(btc.clone(), &address.bech32.unwrap(), 101).unwrap();
    crate::wait_cln_sync!(cln);

    cln.rpc()
        .fundchannel(
            &info.node_id,
            clightningrpc::requests::AmountOrAll::Amount(3000000),
            None,
        )
        .unwrap();

    // looks like that rust is too fast and cln is not able to
    // index all the tx, so this will accept some errors
    wait!(|| {
        let mut channels = cln.rpc().listfunds().unwrap().channels;
        let origin_size = channels.len();
        channels.retain(|chan| chan.state == "CHANNELD_NORMAL");
        if channels.len() == origin_size {
            return Ok(());
        }
        log::info!("{:?}", channels);
        let address = cln.rpc().newaddr(None).unwrap();
        fund_wallet(btc.clone(), &address.bech32.unwrap(), 6).unwrap();
        crate::wait_cln_sync!(cln);
        Err(())
    });
    async_run!(cln.stop()).unwrap();
}

#[test]
pub fn payinvoice_to_lampo() {
    init();

    let mut cln = async_run!(cln::Node::with_params(
        "--developer --dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
        "regtest"
    ))
    .unwrap();
    let btc = cln.btc();
    let lampo_manager = LampoTesting::new(btc.clone()).unwrap();
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
    // mine some bitcoin inside the lampo address
    let address = cln.rpc().newaddr(None).unwrap();
    fund_wallet(btc.clone(), &address.bech32.unwrap(), 101).unwrap();
    crate::wait_cln_sync!(cln);

    cln.rpc()
        .fundchannel(
            &info.node_id,
            clightningrpc::requests::AmountOrAll::Amount(3000000),
            None,
        )
        .unwrap();

    // looks like that rust is too fast and cln is not able to
    // index all the tx, so this will accept some errors
    wait!(|| {
        let mut channels = cln.rpc().listfunds().unwrap().channels;
        let origin_size = channels.len();
        channels.retain(|chan| chan.state == "CHANNELD_NORMAL");
        if channels.len() == origin_size {
            return Ok(());
        }
        log::info!("{:?}", channels);
        let address = cln.rpc().newaddr(None).unwrap();
        fund_wallet(btc.clone(), &address.bech32.unwrap(), 6).unwrap();
        crate::wait_cln_sync!(cln);
        Err(())
    });

    let funds: json::Value = cln.rpc().call("listfunds", json::json!({})).unwrap();
    log::info!("list funds core lightning: {:?}", funds);
    let invoice: response::Invoice = lampo
        .call(
            "invoice",
            json::json!({
                "amount_msat": 100_000_000,
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
        "--developer --dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
        "regtest"
    ))
    .unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let btc = cln.btc();
    let lampo_manager = LampoTesting::new(btc.clone()).unwrap();
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
        "--developer --dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
        "regtest"
    ))
    .unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let btc = cln.btc();
    let lampo_manager = LampoTesting::new(btc.clone()).unwrap();
    let lampo = lampo_manager.lampod();

    let invoce = cln
        .rpc()
        .invoice(
            Some(4000),
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
        "--developer --dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
        "regtest"
    ))
    .unwrap();
    let btc = cln.btc();
    let lampo_manager = LampoTesting::new(btc.clone()).unwrap();
    let lampo = lampo_manager.lampod();

    let events = lampo.events();
    let address = lampo_manager.fund_wallet(101).unwrap();
    wait!(|| {
        let Ok(Event::OnChain(OnChainEvent::NewBestBlock((_, height)))) =
            events.recv_timeout(Duration::from_millis(100))
        else {
            return Err(());
        };
        if height.to_consensus_u32() >= 101 {
            return Ok(());
        }
        Err(())
    });

    let events = lampo.events();
    let _: json::Value = lampo
        .call(
            "fundchannel",
            request::OpenChannel {
                node_id: cln.rpc().getinfo().unwrap().id,
                port: Some(cln.port.into()),
                amount: 500_000_000,
                public: true,
                addr: Some("127.0.0.1".to_owned()),
            },
        )
        // Wait a little bit that the open channel will finish!
        .unwrap();

    let _ = btc.rpc().generate_to_address(6, &address).unwrap();
    wait!(|| {
        log::info!(target: "tests", "wait for confimetion");
        let _ = btc.rpc().generate_to_address(1, &address).unwrap();
        // Get the transaction confirmed
        for _ in 0..100 {
            let Ok(event) = events.recv_timeout(Duration::from_nanos(100)) else {
                continue;
            };
            log::info!(target: "tests", "lampo event: {:?}", event);
            match event {
                Event::Lightning(LightningEvent::ChannelReady { .. }) => return Ok(()),
                _ => continue,
            };
        }
        Err(())
    });

    wait!(|| {
        let channels = cln.rpc().listfunds().unwrap().channels;
        if channels.is_empty() {
            return Err(());
        }

        let mut channels = cln.rpc().listfunds().unwrap().channels;
        let origin_size = channels.len();
        channels.retain(|chan| chan.state == "CHANNELD_NORMAL");
        if channels.len() == origin_size {
            return Ok(());
        }

        let channels: response::Channels = lampo.call("channels", json::json!({})).unwrap();
        if !channels.channels.first().unwrap().ready {
            return Err(());
        }
        let address = cln.rpc().newaddr(None).unwrap();
        fund_wallet(btc.clone(), &address.bech32.unwrap(), 1).unwrap();
        crate::wait_cln_sync!(cln);
        Err(())
    });

    let invoce = cln
        .rpc()
        .invoice(
            Some(100_000_000),
            "lampo",
            "need to be decoded by lampo",
            None,
            None,
            None,
        )
        .unwrap();

    log::info!(target: "tests", "{:?}", cln.rpc().listfunds());
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

#[test]
fn be_able_to_kesend_payments() {
    init();
    let mut cln = async_run!(cln::Node::with_params(
        "--developer --dev-bitcoind-poll=1 --dev-fast-gossip --dev-allow-localhost",
        "regtest"
    ))
    .unwrap();
    std::thread::sleep(Duration::from_secs(2));
    let btc = cln.btc();
    let lampo_manager = LampoTesting::new(btc.clone()).unwrap();
    let lampo = lampo_manager.lampod();
    let _info: response::GetInfo = lampo.call("getinfo", json::json!({})).unwrap();
    let info_cln = cln.rpc().getinfo().unwrap();
    let events = lampo.events();
    let address = lampo_manager.fund_wallet(101).unwrap();
    wait!(|| {
        let Ok(Event::OnChain(OnChainEvent::NewBestBlock((_, height)))) =
            events.recv_timeout(Duration::from_millis(100))
        else {
            return Err(());
        };
        if height.to_consensus_u32() == 101 {
            return Ok(());
        }
        Err(())
    });
    let _: json::Value = lampo
        .call(
            "fundchannel",
            request::OpenChannel {
                node_id: cln.rpc().getinfo().unwrap().id,
                port: Some(cln.port.into()),
                amount: 1_500_000_000,
                public: true,
                addr: Some("127.0.0.1".to_owned()),
            },
        )
        .unwrap();

    std::thread::sleep(Duration::from_secs(2));

    // Get the transaction confirmed
    let _ = btc.rpc().generate_to_address(6, &address).unwrap();
    wait!(|| {
        log::info!(target: "tests", "wait for confimetion");
        let _ = btc.rpc().generate_to_address(1, &address).unwrap();
        // Get the transaction confirmed
        for _ in 0..100 {
            let Ok(event) = events.recv_timeout(Duration::from_nanos(100)) else {
                continue;
            };
            log::info!(target: "tests", "lampo event: {:?}", event);
            match event {
                Event::Lightning(LightningEvent::ChannelReady { .. }) => return Ok(()),
                _ => continue,
            };
        }
        Err(())
    });

    wait!(|| {
        let channels = cln.rpc().listfunds().unwrap().channels;
        if channels.is_empty() {
            return Err(());
        }

        let mut channels = cln.rpc().listfunds().unwrap().channels;
        let origin_size = channels.len();
        channels.retain(|chan| chan.state == "CHANNELD_NORMAL");
        if channels.len() == origin_size {
            return Ok(());
        }

        let channels: response::Channels = lampo.call("channels", json::json!({})).unwrap();
        if !channels.channels.first().unwrap().ready {
            return Err(());
        }
        let address = cln.rpc().newaddr(None).unwrap();
        fund_wallet(btc.clone(), &address.bech32.unwrap(), 1).unwrap();
        crate::wait_cln_sync!(cln);
        Err(())
    });

    let result: error::Result<json::Value> = lampo.call(
        "keysend",
        request::KeySend {
            destination: PublicKey::from_str(info_cln.id.as_str()).unwrap(),
            amount_msat: 100_00_000,
        },
    );
    assert!(result.is_ok(), "{:?}", result);
    async_run!(cln.stop()).unwrap();
}
