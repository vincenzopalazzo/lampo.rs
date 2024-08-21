//! Integration tests between lampo nodes.
//!
//! Author: Vincenzo Palazzo <vincenzopalazzo@member.fsf.org>
use std::borrow::Borrow;
use std::error::Error;
use std::ops::Deref;
use std::os::unix::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use lampo_common::error;
use lampo_common::event::liquidity::LiquidityEvent;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::onchain::OnChainEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json;
use lampo_common::ldk::ln::msgs::SocketAddress;
use lampo_common::model::{request, response};

use lampo_common::secp256k1::PublicKey;
use lampo_testing::prelude::*;
use lampo_testing::wait;
use lampo_testing::LampoTesting;
use lampod::ln::liquidity;

use crate::init;

#[test]
pub fn init_connection_test_between_lampo() -> error::Result<()> {
    init();
    let btc = async_run!(btc::BtcNode::tmp("regtest"))?;
    let btc = Arc::new(btc);
    let node1 = LampoTesting::new(btc.clone())?;
    let node2 = LampoTesting::new(btc.clone())?;
    let response: response::Connect = node2
        .lampod()
        .call(
            "connect",
            request::Connect {
                node_id: node1.info.node_id,
                addr: "127.0.0.1".to_owned(),
                port: node1.port,
            },
        )
        .unwrap();
    log::debug!("node 1 -> connected with node 2 {:?}", response);
    Ok(())
}

#[test]
pub fn fund_a_simple_channel_from() -> error::Result<()> {
    init();
    let btc = async_run!(btc::BtcNode::tmp("regtest"))?;
    let btc = Arc::new(btc);
    let node1 = Arc::new(LampoTesting::new(btc.clone())?);
    let node2 = Arc::new(LampoTesting::new(btc.clone())?);
    let response: response::Connect = node2
        .lampod()
        .call(
            "connect",
            request::Connect {
                node_id: node1.info.node_id.clone(),
                addr: "127.0.0.1".to_owned(),
                port: node1.port,
            },
        )
        .unwrap();
    log::debug!("node 1 -> connected with node 2 {:?}", response);

    let events = node1.lampod().events();
    let _ = node1.fund_wallet(101).unwrap();
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

    let response: json::Value = node1
        .lampod()
        .call(
            "fundchannel",
            request::OpenChannel {
                node_id: node2.info.node_id.clone(),
                amount: 100000,
                public: true,
                port: None,
                addr: None,
            },
        )
        .unwrap();
    assert!(response.get("tx").is_some());

    let events = node2.lampod().events();
    wait!(|| {
        while let Ok(event) = events.recv_timeout(Duration::from_millis(10)) {
            node2.fund_wallet(1).unwrap();
            if let Event::Lightning(LightningEvent::ChannelReady {
                counterparty_node_id,
                ..
            }) = event
            {
                if counterparty_node_id.to_string() == node1.info.node_id {
                    return Err(());
                }
                return Ok(());
            };
            // check if lampo see the channel
            let channels: response::Channels =
                node2.lampod().call("channels", json::json!({})).unwrap();
            if !channels.channels.is_empty() {
                return Ok(());
            }

            if channels.channels.first().unwrap().ready {
                return Ok(());
            }
        }
        node2.fund_wallet(6).unwrap();
        Err(())
    });
    Ok(())
}

#[test]
pub fn pay_invoice_simple_case_lampo() -> error::Result<()> {
    init();
    let btc = async_run!(btc::BtcNode::tmp("regtest"))?;
    let btc = Arc::new(btc);
    let node1 = Arc::new(LampoTesting::new(btc.clone())?);
    let node2 = Arc::new(LampoTesting::new(btc.clone())?);

    let events = node1.lampod().events();
    let _ = node1.fund_wallet(101)?;
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

    let response: json::Value = node1
        .lampod()
        .call(
            "fundchannel",
            request::OpenChannel {
                node_id: node2.info.node_id.clone(),
                amount: 1_000_000,
                public: true,
                addr: Some("127.0.0.1".to_owned()),
                port: Some(node2.port),
            },
        )
        .unwrap();
    assert!(response.get("tx").is_some());

    wait!(|| {
        while let Ok(event) = events.recv_timeout(Duration::from_millis(10)) {
            node2.fund_wallet(6).unwrap();
            if let Event::Lightning(LightningEvent::ChannelReady {
                counterparty_node_id,
                ..
            }) = event
            {
                if counterparty_node_id.to_string() == node1.info.node_id {
                    return Err(());
                }
                return Ok(());
            };
            // check if lampo see the channel
            let channels: response::Channels =
                node2.lampod().call("channels", json::json!({})).unwrap();
            if channels.channels.is_empty() {
                return Err(());
            }

            if !channels.channels.first().unwrap().ready {
                return Err(());
            }

            let channels: response::Channels =
                node1.lampod().call("channels", json::json!({})).unwrap();

            if channels.channels.is_empty() {
                return Err(());
            }

            if channels.channels.first().unwrap().ready {
                return Ok(());
            }
        }
        node2.fund_wallet(6).unwrap();
        Err(())
    });

    let invoice: response::Invoice = node2.lampod().call(
        "invoice",
        request::GenerateInvoice {
            description: "making sure that we can work betwen lampo version".to_owned(),
            amount_msat: Some(100_000_000),
            expiring_in: None,
        },
    )?;

    log::info!(target: &node2.info.node_id, "invoice generated `{:?}`", invoice);

    let pay: response::PayResult = node1.lampod().call(
        "pay",
        request::Pay {
            invoice_str: invoice.bolt11,
            amount: None,
        },
    )?;
    log::info!(target: &node1.info.node_id, "payment made `{:?}`", pay);
    Ok(())
}

#[test]
pub fn pay_offer_simple_case_lampo() -> error::Result<()> {
    init();
    let btc = async_run!(btc::BtcNode::tmp("regtest"))?;
    let btc = Arc::new(btc);
    let node1 = Arc::new(LampoTesting::new(btc.clone())?);
    let node2 = Arc::new(LampoTesting::new(btc.clone())?);

    let events = node1.lampod().events();
    let _ = node1.fund_wallet(101)?;
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

    let response: json::Value = node1
        .lampod()
        .call(
            "fundchannel",
            request::OpenChannel {
                node_id: node2.info.node_id.clone(),
                amount: 1_000_000,
                public: true,
                addr: Some("127.0.0.1".to_owned()),
                port: Some(node2.port),
            },
        )
        .unwrap();
    assert!(response.get("tx").is_some());

    wait!(|| {
        while let Ok(event) = events.recv_timeout(Duration::from_millis(10)) {
            node2.fund_wallet(6).unwrap();
            if let Event::Lightning(LightningEvent::ChannelReady {
                counterparty_node_id,
                ..
            }) = event
            {
                if counterparty_node_id.to_string() == node1.info.node_id {
                    return Err(());
                }
                return Ok(());
            };
            // check if lampo see the channel
            let channels: response::Channels =
                node2.lampod().call("channels", json::json!({})).unwrap();
            if channels.channels.is_empty() {
                return Err(());
            }

            if !channels.channels.first().unwrap().ready {
                return Err(());
            }

            let channels: response::Channels =
                node1.lampod().call("channels", json::json!({})).unwrap();

            if channels.channels.is_empty() {
                return Err(());
            }

            if channels.channels.first().unwrap().ready {
                return Ok(());
            }
        }
        node2.fund_wallet(6).unwrap();
        Err(())
    });

    let offer: response::Offer = node2.lampod().call(
        "offer",
        request::GenerateOffer {
            description: Some("making sure that we can work betwen lampo version".to_owned()),
            amount_msat: Some(100_000_000),
        },
    )?;

    log::info!(target: &node2.info.node_id, "offer generated `{:?}`", offer);

    let pay: response::PayResult = node1.lampod().call(
        "pay",
        request::Pay {
            invoice_str: offer.bolt12,
            amount: None,
        },
    )?;
    log::info!(target: &node1.info.node_id, "payment made `{:?}`", pay);
    Ok(())
}

#[test]
pub fn pay_offer_minimal_offer() -> error::Result<()> {
    init();
    let btc = async_run!(btc::BtcNode::tmp("regtest"))?;
    let btc = Arc::new(btc);
    let node1 = Arc::new(LampoTesting::new(btc.clone())?);
    let node2 = Arc::new(LampoTesting::new(btc.clone())?);

    let events = node1.lampod().events();
    let _ = node1.fund_wallet(101)?;
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

    let response: json::Value = node1
        .lampod()
        .call(
            "fundchannel",
            request::OpenChannel {
                node_id: node2.info.node_id.clone(),
                amount: 1_000_000,
                public: true,
                addr: Some("127.0.0.1".to_owned()),
                port: Some(node2.port),
            },
        )
        .unwrap();
    assert!(response.get("tx").is_some());

    wait!(|| {
        while let Ok(event) = events.recv_timeout(Duration::from_millis(10)) {
            node2.fund_wallet(6).unwrap();
            if let Event::Lightning(LightningEvent::ChannelReady {
                counterparty_node_id,
                ..
            }) = event
            {
                if counterparty_node_id.to_string() == node1.info.node_id {
                    return Err(());
                }
                return Ok(());
            };
            // check if lampo see the channel
            let channels: response::Channels =
                node2.lampod().call("channels", json::json!({})).unwrap();
            if channels.channels.is_empty() {
                return Err(());
            }

            if !channels.channels.first().unwrap().ready {
                return Err(());
            }

            let channels: response::Channels =
                node1.lampod().call("channels", json::json!({})).unwrap();

            if channels.channels.is_empty() {
                return Err(());
            }

            if channels.channels.first().unwrap().ready {
                return Ok(());
            }
        }
        node2.fund_wallet(6).unwrap();
        Err(())
    });

    let offer: response::Offer = node2.lampod().call(
        "offer",
        request::GenerateOffer {
            description: None,
            amount_msat: None,
        },
    )?;

    log::info!(target: &node2.info.node_id, "offer generated `{:?}`", offer);

    let pay: response::PayResult = node1.lampod().call(
        "pay",
        request::Pay {
            invoice_str: offer.bolt12,
            amount: Some(100_000_000),
        },
    )?;
    log::info!(target: &node1.info.node_id, "payment made `{:?}`", pay);
    Ok(())
}

#[test]
pub fn decode_offer() -> error::Result<()> {
    init();
    let btc = async_run!(btc::BtcNode::tmp("regtest"))?;
    let btc = Arc::new(btc);
    let node1 = Arc::new(LampoTesting::new(btc.clone())?);
    let node2 = Arc::new(LampoTesting::new(btc.clone())?);

    let events = node1.lampod().events();
    let _ = node1.fund_wallet(101)?;
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

    let response: json::Value = node1
        .lampod()
        .call(
            "fundchannel",
            request::OpenChannel {
                node_id: node2.info.node_id.clone(),
                amount: 1_000_000,
                public: true,
                addr: Some("127.0.0.1".to_owned()),
                port: Some(node2.port),
            },
        )
        .unwrap();
    assert!(response.get("tx").is_some());

    wait!(|| {
        while let Ok(event) = events.recv_timeout(Duration::from_millis(10)) {
            node2.fund_wallet(6).unwrap();
            if let Event::Lightning(LightningEvent::ChannelReady {
                counterparty_node_id,
                ..
            }) = event
            {
                if counterparty_node_id.to_string() == node1.info.node_id {
                    return Err(());
                }
                return Ok(());
            };
            // check if lampo see the channel
            let channels: response::Channels =
                node2.lampod().call("channels", json::json!({})).unwrap();
            if channels.channels.is_empty() {
                return Err(());
            }

            if !channels.channels.first().unwrap().ready {
                return Err(());
            }

            let channels: response::Channels =
                node1.lampod().call("channels", json::json!({})).unwrap();

            if channels.channels.is_empty() {
                return Err(());
            }

            if channels.channels.first().unwrap().ready {
                return Ok(());
            }
        }
        node2.fund_wallet(6).unwrap();
        Err(())
    });

    let offer: response::Offer = node2.lampod().call(
        "offer",
        request::GenerateOffer {
            description: None,
            amount_msat: None,
        },
    )?;

    log::info!(target: &node2.info.node_id, "offer generated `{:?}`", offer);

    let decode: response::InvoiceInfo = node2.lampod().call(
        "decode",
        request::DecodeInvoice {
            invoice_str: offer.bolt12,
        },
    )?;

    assert_eq!(decode.issuer_id, Some(node2.info.node_id.clone()));
    log::info!(target: &node2.info.node_id, "decode offer `{:?}`", decode);
    Ok(())
}

#[test]
pub fn test_generate_a_jit_invoice() -> error::Result<()> {
    init();
    let btc = async_run!(btc::BtcNode::tmp("regtest"))?;
    let btc = Arc::new(btc);
    // This is acting as a server
    let node1 = Arc::new(LampoTesting::new_liquidity(
        btc.clone(),
        "server".to_string(),
        None,
        None,
    )?);

    let _info: response::GetInfo = node1.lampod().call("getinfo", json::json!({})).unwrap();
    let node1_id = _info.node_id.clone();
    let socket_addr = format!("127.0.0.1:{}", node1.port.clone());
    let socket_addr =
        SocketAddress::from_str(&socket_addr).expect("Failed to parse socket address");

    // This should act as a client
    let node2 = Arc::new(LampoTesting::new_liquidity(
        btc.clone(),
        "consumer".to_string(),
        Some(node1.info.node_id.clone()),
        Some(socket_addr.to_string()),
    )?);

    let response: response::Connect = node2
        .lampod()
        .call(
            "connect",
            request::Connect {
                node_id: node1.info.node_id.clone(),
                addr: "127.0.0.1".to_owned(),
                port: node1.port,
            },
        )
        .unwrap();

    log::info!("Connect successful: {:?}", response);

    let events = node1.lampod().events();
    let _ = node1.fund_wallet(101)?;
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

    let result = node2
        .liquidity()
        .create_jit_invoice(10000, "A new desc".to_string());

    assert!(result.is_ok());
    log::info!("{}", result.unwrap().to_string());

    Ok(())
}

#[test]
pub fn test_pay_a_jit_invoice() -> error::Result<()> {
    init();
    let btc = async_run!(btc::BtcNode::tmp("regtest"))?;
    let btc = Arc::new(btc);
    // This is acting as a server
    let node1 = Arc::new(LampoTesting::new_liquidity(
        btc.clone(),
        "server".to_string(),
        None,
        None,
    )?);

    let _info: response::GetInfo = node1.lampod().call("getinfo", json::json!({})).unwrap();
    let node1_id = _info.node_id.clone();
    let socket_addr = format!("127.0.0.1:{}", node1.port.clone());
    let socket_addr =
        SocketAddress::from_str(&socket_addr).expect("Failed to parse socket address");

    // This should act as a client
    let node2 = Arc::new(LampoTesting::new_liquidity(
        btc.clone(),
        "consumer".to_string(),
        Some(node1.info.node_id.clone()),
        Some(socket_addr.to_string()),
    )?);

    // This is a payee
    let node3 = Arc::new(LampoTesting::new(btc.clone())?);

    // Connecting server and client
    let response: response::Connect = node2
        .lampod()
        .call(
            "connect",
            request::Connect {
                node_id: node1.info.node_id.clone(),
                addr: "127.0.0.1".to_owned(),
                port: node1.port,
            },
        )
        .unwrap();

    // The Provider needs to pay this invoice
    let result = node2
        .liquidity()
        .create_jit_invoice(1000000, "A new desc".to_string())?;

    // Connecting server and payee
    let response: response::Connect = node1
        .lampod()
        .call(
            "connect",
            request::Connect {
                node_id: node3.info.node_id.clone(),
                addr: "127.0.0.1".to_owned(),
                port: node3.port,
            },
        )
        .unwrap();

    log::info!("Connect successful: {:?}", response);

    let node_id = PublicKey::from_str(&node1_id).expect("Wrong node id");

    let events = node1.lampod().events();
    let _ = node1.fund_wallet(101)?;
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

    let _ = node3.fund_wallet(101)?;
    let events3 = node3.lampod().events();
    wait!(|| {
        let Ok(Event::OnChain(OnChainEvent::NewBestBlock((_, height)))) =
            events3.recv_timeout(Duration::from_millis(100))
        else {
            return Err(());
        };
        if height.to_consensus_u32() == 202 {
            return Ok(());
        }
        Err(())
    });

    log::info!("This is the invoice: {}", result.clone().to_string());

    // Funding channel between payee and server
    let response: json::Value = node3
        .lampod()
        .call(
            "fundchannel",
            request::OpenChannel {
                node_id: node1.info.node_id.clone(),
                amount: 10000000,
                public: true,
                port: Some(node1.port.clone()),
                addr: Some("127.0.0.1".to_string()),
            },
        )
        .unwrap();

    let events3 = node3.lampod().events();
    wait!(|| {
        while let Ok(event) = events3.recv_timeout(Duration::from_millis(10)) {
            node3.fund_wallet(6).unwrap();
            if let Event::Lightning(LightningEvent::ChannelReady {
                counterparty_node_id,
                ..
            }) = event
            {
                if counterparty_node_id.to_string() == node1.info.node_id {
                    return Err(());
                }
                return Ok(());
            };
            // check if lampo see the channel
            let channels: response::Channels =
                node3.lampod().call("channels", json::json!({})).unwrap();
            if channels.channels.is_empty() {
                return Err(());
            }

            if !channels.channels.first().unwrap().ready {
                return Err(());
            }

            if channels.channels.first().unwrap().ready {
                return Ok(());
            }
        }
        node3.fund_wallet(6).unwrap();
        Err(())
    });
    // This would return a timeout as json_pay will continue to wait till it get an
    // PaymentSuccessful event. Out logic is not at fault here it is because node1 gets
    // the HTLCIntercepted event but can't process further because of this function blocking
    // the thread
    // TODO: Find a better way to fix this.
    // There are three nodes payer <-> lsp <-> client
    // The JIT channels are created on the go when the payer pays the invoice generated by the LSP.
    // But as we are generating the invoice and paying on the go, the Channel remains pending as we are not
    // by hand generating any blocks in order to get this channel confirmed.
    let pay: response::PayResult = node3.lampod().call(
        "pay",
        request::Pay {
            invoice_str: result.to_string(),
            amount: result.clone().amount_milli_satoshis(),
        },
    )?;

    Ok(())
}
