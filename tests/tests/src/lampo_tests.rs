//! Integration tests between lampo nodes.
//!
//! Author: Vincenzo Palazzo <vincenzopalazzo@member.fsf.org>
use std::sync::Arc;
use std::time::Duration;

use lampo_common::error;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::onchain::OnChainEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json;
use lampo_common::model::{request, response};

use lampo_testing::async_wait;
use lampo_testing::prelude::*;
use lampo_testing::wait;
use lampo_testing::LampoTesting;

use crate::init;

#[tokio::test]
pub async fn init_connection_test_between_lampo() -> error::Result<()> {
    init();
    let btc = btc::BtcNode::tmp("regtest").await?;
    let btc = Arc::new(btc);
    let node1 = LampoTesting::new(btc.clone()).await?;
    let node2 = LampoTesting::new(btc.clone()).await?;
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
        .await?;
    log::debug!("node 1 -> connected with node 2 {:?}", response);
    Ok(())
}

#[tokio::test]
pub async fn fund_a_simple_channel_from() -> error::Result<()> {
    init();
    let btc = btc::BtcNode::tmp("regtest").await?;
    let btc = Arc::new(btc);
    let node1 = Arc::new(LampoTesting::new(btc.clone()).await?);
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);
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
        .await?;
    log::debug!("node 1 -> connected with node 2 {:?}", response);

    let events = node1.lampod().events();
    let _ = node1.fund_wallet(101).await?;
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
        .await?;
    assert!(response.get("tx").is_some());

    let events = node2.lampod().events();
    async_wait!(async {
        while let Ok(event) = events.recv_timeout(Duration::from_millis(10)) {
            node2.fund_wallet(1).await.unwrap();
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
            let channels: response::Channels = node2
                .lampod()
                .call("channels", json::json!({}))
                .await
                .unwrap();
            if !channels.channels.is_empty() {
                return Ok(());
            }

            if channels.channels.first().unwrap().ready {
                return Ok(());
            }
        }
        node2.fund_wallet(6).await.unwrap();
        Err(())
    });
    Ok(())
}

#[tokio::test]
pub async fn pay_invoice_simple_case_lampo() -> error::Result<()> {
    init();
    let btc = btc::BtcNode::tmp("regtest").await?;
    let btc = Arc::new(btc);
    let node1 = Arc::new(LampoTesting::new(btc.clone()).await?);
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);

    let events = node1.lampod().events();
    let _ = node1.fund_wallet(101).await?;
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
        .await?;
    assert!(response.get("tx").is_some());

    async_wait!(async {
        while let Ok(event) = events.recv_timeout(Duration::from_millis(10)) {
            node2.fund_wallet(6).await.unwrap();
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
            let channels: response::Channels = node2
                .lampod()
                .call("channels", json::json!({}))
                .await
                .unwrap();
            if channels.channels.is_empty() {
                return Err(());
            }

            if !channels.channels.first().unwrap().ready {
                return Err(());
            }

            let channels: response::Channels = node1
                .lampod()
                .call("channels", json::json!({}))
                .await
                .unwrap();

            if channels.channels.is_empty() {
                return Err(());
            }

            if channels.channels.first().unwrap().ready {
                return Ok(());
            }
        }
        node2.fund_wallet(6).await.unwrap();
        Err(())
    });

    let invoice: response::Invoice = node2
        .lampod()
        .call(
            "invoice",
            request::GenerateInvoice {
                description: "making sure that we can work betwen lampo version".to_owned(),
                amount_msat: Some(100_000_000),
                expiring_in: None,
            },
        )
        .await?;

    log::info!(target: &node2.info.node_id, "invoice generated `{:?}`", invoice);

    let pay: response::PayResult = node1
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: invoice.bolt11,
                amount: None,
            },
        )
        .await?;
    log::info!(target: &node1.info.node_id, "payment made `{:?}`", pay);
    Ok(())
}

#[tokio::test]
pub async fn pay_offer_simple_case_lampo() -> error::Result<()> {
    init();
    let btc = btc::BtcNode::tmp("regtest").await?;
    let btc = Arc::new(btc);
    let node1 = Arc::new(LampoTesting::new(btc.clone()).await?);
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);

    let events = node1.lampod().events();
    let _ = node1.fund_wallet(101).await?;
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
        .await?;
    assert!(response.get("tx").is_some());

    async_wait!(async {
        while let Ok(event) = events.recv_timeout(Duration::from_millis(10)) {
            node2.fund_wallet(6).await.unwrap();
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
            let channels: response::Channels = node2
                .lampod()
                .call("channels", json::json!({}))
                .await
                .unwrap();
            if channels.channels.is_empty() {
                return Err(());
            }

            if !channels.channels.first().unwrap().ready {
                return Err(());
            }

            let channels: response::Channels = node1
                .lampod()
                .call("channels", json::json!({}))
                .await
                .unwrap();

            if channels.channels.is_empty() {
                return Err(());
            }

            if channels.channels.first().unwrap().ready {
                return Ok(());
            }
        }
        node2.fund_wallet(6).await.unwrap();
        Err(())
    });

    let offer: response::Offer = node2
        .lampod()
        .call(
            "offer",
            request::GenerateOffer {
                description: Some("making sure that we can work betwen lampo version".to_owned()),
                amount_msat: Some(100_000_000),
            },
        )
        .await?;

    log::info!(target: &node2.info.node_id, "offer generated `{:?}`", offer);

    let pay: response::PayResult = node1
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: offer.bolt12,
                amount: None,
            },
        )
        .await?;
    log::info!(target: &node1.info.node_id, "payment made `{:?}`", pay);
    Ok(())
}

#[tokio::test]
pub async fn pay_offer_minimal_offer() -> error::Result<()> {
    init();
    let btc = btc::BtcNode::tmp("regtest").await?;
    let btc = Arc::new(btc);
    let node1 = Arc::new(LampoTesting::new(btc.clone()).await?);
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);

    let events = node1.lampod().events();
    let _ = node1.fund_wallet(101).await?;
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
        .await?;
    assert!(response.get("tx").is_some());

    async_wait!(async {
        while let Ok(event) = events.recv_timeout(Duration::from_millis(10)) {
            node2.fund_wallet(6).await.unwrap();
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
            let channels: response::Channels = node2
                .lampod()
                .call("channels", json::json!({}))
                .await
                .unwrap();
            if channels.channels.is_empty() {
                return Err(());
            }

            if !channels.channels.first().unwrap().ready {
                return Err(());
            }

            let channels: response::Channels = node1
                .lampod()
                .call("channels", json::json!({}))
                .await
                .unwrap();

            if channels.channels.is_empty() {
                return Err(());
            }

            if channels.channels.first().unwrap().ready {
                return Ok(());
            }
        }
        node2.fund_wallet(6).await.unwrap();
        Err(())
    });

    let offer: response::Offer = node2
        .lampod()
        .call(
            "offer",
            request::GenerateOffer {
                description: None,
                amount_msat: None,
            },
        )
        .await?;

    log::info!(target: &node2.info.node_id, "offer generated `{:?}`", offer);

    let pay: response::PayResult = node1
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: offer.bolt12,
                amount: Some(100_000_000),
            },
        )
        .await?;
    log::info!(target: &node1.info.node_id, "payment made `{:?}`", pay);
    Ok(())
}

#[tokio::test]
pub async fn decode_offer() -> error::Result<()> {
    init();
    let btc = btc::BtcNode::tmp("regtest").await?;
    let btc = Arc::new(btc);
    let node1 = Arc::new(LampoTesting::new(btc.clone()).await?);
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);

    let events = node1.lampod().events();
    let _ = node1.fund_wallet(101).await?;
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
        .await?;
    assert!(response.get("tx").is_some());

    async_wait!(async {
        while let Ok(event) = events.recv_timeout(Duration::from_millis(10)) {
            node2.fund_wallet(6).await.unwrap();
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
            let channels: response::Channels = node2
                .lampod()
                .call("channels", json::json!({}))
                .await
                .unwrap();
            if channels.channels.is_empty() {
                return Err(());
            }

            if !channels.channels.first().unwrap().ready {
                return Err(());
            }

            let channels: response::Channels = node1
                .lampod()
                .call("channels", json::json!({}))
                .await
                .unwrap();

            if channels.channels.is_empty() {
                return Err(());
            }

            if channels.channels.first().unwrap().ready {
                return Ok(());
            }
        }
        node2.fund_wallet(6).await.unwrap();
        Err(())
    });

    let offer: response::Offer = node2
        .lampod()
        .call(
            "offer",
            request::GenerateOffer {
                description: None,
                amount_msat: None,
            },
        )
        .await?;

    log::info!(target: &node2.info.node_id, "offer generated `{:?}`", offer);

    let decode: response::InvoiceInfo = node2
        .lampod()
        .call(
            "decode",
            request::DecodeInvoice {
                invoice_str: offer.bolt12,
            },
        )
        .await?;

    assert_eq!(decode.issuer_id, Some(node2.info.node_id.clone()));
    log::info!(target: &node2.info.node_id, "decode offer `{:?}`", decode);
    Ok(())
}
