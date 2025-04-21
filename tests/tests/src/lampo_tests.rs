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

use lampo_testing::LampoTesting;
use lampo_testing::{async_wait, prelude::*};

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

use crate::init;

async fn recv_event_timeout(
    receiver: &mut UnboundedReceiver<Event>,
    duration: Duration,
) -> Result<Event, String> {
    match timeout(duration, receiver.recv()).await {
        Ok(Some(event)) => Ok(event),
        Ok(None) => Err("Channel closed".to_string()),
        Err(_) => Err("Timeout".to_string()),
    }
}

#[tokio_test_shutdown_timeout::test(60)]
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
        .await
        .unwrap();
    log::debug!("node 1 -> connected with node 2 {:?}", response);
    Ok(())
}

#[ignore = "For some reason this test is blocking, so this need a better looking"]
#[tokio_test_shutdown_timeout::test(160)]
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
        .await
        .unwrap();
    log::debug!("node 1 -> connected with node 2 {:?}", response);

    let mut events_node1 = node1.lampod().events();
    let _ = node1.fund_wallet(101).await.unwrap();
    async_wait!(async {
        match recv_event_timeout(&mut events_node1, Duration::from_millis(100)).await {
            Ok(Event::OnChain(OnChainEvent::NewBestBlock((_, height)))) => {
                if height.to_consensus_u32() == 101 {
                    return Ok(());
                }
            }
            Ok(_) => { /* Ignorined other events */ }
            Err(e) => log::trace!("recv_event_timeout error: {}", e),
        }
        Err::<(), ()>(()) // Using generic Err type for async_wait macro
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
        .await
        .unwrap();
    assert!(response.get("tx").is_some());

    let mut events_node2 = node2.lampod().events();
    async_wait!(
        async {
            match recv_event_timeout(&mut events_node2, Duration::from_millis(10)).await {
                Ok(event) => {
                    if let Event::Lightning(LightningEvent::ChannelReady {
                        counterparty_node_id,
                        ..
                    }) = event
                    {
                        // it should succeed if the counterparty IS node1
                        if counterparty_node_id.to_string() == node1.info.node_id {
                            log::info!("ChannelReady event received from correct peer.");
                            // Additional check: Ensure channel is listed and ready
                            let channels: response::Channels = node2
                                .lampod()
                                .call("channels", json::json!({}))
                                .await
                                .unwrap();
                            if !channels.channels.is_empty()
                                && channels.channels.first().unwrap().ready
                            {
                                log::info!("Channel confirmed ready in listchannels.");
                                return Ok(());
                            } else {
                                log::warn!("ChannelReady event received, but channel not ready in listchannels yet.");
                            }
                        } else {
                            log::warn!(
                                "ChannelReady received, but from unexpected peer: {}",
                                counterparty_node_id
                            );
                        }
                    } else {
                        log::trace!("Received other event: {:?}", event);
                    }
                }
                Err(ref msg) if msg == "Timeout" => {
                    log::trace!("No event received, mining block...");
                }
                Err(ref msg) => {
                    log::error!("Error receiving event: {}", msg);
                    return Err(());
                }
            }

            node2.fund_wallet(1).await.unwrap();
            Err::<(), ()>(())
        },
        10
    );

    Ok(())
}

/*
#[ignore]
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

#[ignore]
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

#[ignore]
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

#[ignore]
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

    let decode: response::Bolt12InvoiceInfo = node2.lampod().call(
        "decode",
        request::DecodeInvoice {
            invoice_str: offer.bolt12,
        },
    )?;

    assert_eq!(decode.issuer_id.clone(), Some(node2.info.node_id.clone()));
    log::info!(target: &node2.info.node_id, "decode offer `{:?}`", decode);
    Ok(())
}

#[ignore]
#[test]
pub fn decode_offer_hex() -> error::Result<()> {
    init();
    let btc = async_run!(btc::BtcNode::tmp("regtest"))?;
    let btc = Arc::new(btc);
    let node1 = Arc::new(LampoTesting::new(btc.clone())?);

    // For now I am hardcoding this offer as generating an `offer` from test is broken at this point.
    let decode: response::Bolt12InvoiceInfo = node1.lampod().call(
        "decode",
        request::DecodeInvoice {
            invoice_str: "lno1qgsyxjtl6luzd9t3pr62xr7eemp6awnejusgf6gw45q75vcfqqqqqqqsespexwyy4tcadvgg89l9aljus6709kx235hhqrk6n8dey98uyuftzdqzrtkahuum7m56dxlnx8r6tffy54004l7kvs7pylmxx7xs4n54986qyqeeuhhunayntt50snmdkq4t7fzsgghpl69v9csgparek8kv7dlp5uqr8ymp5s4z9upmwr2s8xu020d45t5phqc8nljrq8gzsjmurzevawjz6j6rc95xwfvnhgfx6v4c3jha7jwynecrz3y092nn25ek4yl7xp9yu9ry9zqagt0ktn4wwvqg52v9ss9ls22sqyqqestzp2l6decpn87pq96udsvx".to_string(),
        },
    )?;

    assert_eq!(
        decode.offer_id,
        "34460869549e37748ceaabdcff6284a98266c18052ab2a7e9eb5a1af0a5e5b7d"
    );
    Ok(())
}
*/
