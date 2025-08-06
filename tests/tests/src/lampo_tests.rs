//! Integration tests between lampo nodes.
//!
//! Author: Vincenzo Palazzo <vincenzopalazzo@member.fsf.org>
use std::sync::Arc;

use lampo_common::error;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json;
use lampo_common::model::{request, response};

use lampo_testing::LampoTesting;
use lampo_testing::{async_wait, prelude::*};

use crate::init;

#[tokio_test_shutdown_timeout::test(1)]
pub async fn init_connection_test_between_lampo() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let node2 = LampoTesting::new(node1.btc.clone()).await?;
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

#[tokio_test_shutdown_timeout::test(5)]
pub async fn fund_a_simple_channel_from() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let btc = node1.btc.clone();
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

    let mut events = node2.lampod().events();
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
    node2.fund_wallet(10).await.unwrap();

    async_wait!(async {
        while let Some(event) = events.recv().await {
            log::info!(target: "tests", "Event received {:?}", event);
            if let Event::Lightning(LightningEvent::ChannelReady {
                counterparty_node_id,
                ..
            }) = event
            {
                if counterparty_node_id.to_string() != node1.info.node_id {
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

#[tokio_test_shutdown_timeout::test(5)]
pub async fn pay_invoice_simple_case_lampo() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let btc = node1.btc.clone();
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);

    // There is a channel node1 -> node2
    node1.fund_channel_with(node2.clone(), 100_000_000).await?;

    let invoice: response::Invoice = node2
        .lampod()
        .call(
            "invoice",
            request::GenerateInvoice {
                description: "making sure that we can work betwen lampo version".to_owned(),
                amount_msat: Some(100_000),
                expiring_in: None,
            },
        )
        .await?;

    log::info!(target: &node1.info.node_id, "invoice generated `{:?}`", invoice);

    let pay: response::PayResult = node1
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: invoice.bolt11,
                amount: None,
                bolt12: None,
            },
        )
        .await?;
    log::info!(target: &node2.info.node_id, "payment made `{:?}`", pay);
    Ok(())
}

#[tokio_test_shutdown_timeout::test(5)]
pub async fn pay_offer_simple_case_lampo() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let btc = node1.btc.clone();
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);

    // There is a channel node1 -> node2
    node1.fund_channel_with(node2.clone(), 100_000_000).await?;

    let offer: response::Offer = node2
        .lampod()
        .call(
            "offer",
            request::GenerateOffer {
                description: Some("making sure that we can work betwen lampo version".to_owned()),
                amount_msat: Some(100_000),
            },
        )
        .await?;

    log::info!(target: &node1.info.node_id, "offer generated `{:?}`", offer);

    let pay: response::PayResult = node1
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: offer.bolt12,
                amount: None,
                bolt12: None,
            },
        )
        .await?;
    log::info!(target: &node2.info.node_id, "payment made `{:?}`", pay);
    Ok(())
}

#[tokio_test_shutdown_timeout::test(10)]
pub async fn pay_offer_minimal_offer() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let btc = node1.btc.clone();
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);

    node1.fund_channel_with(node2.clone(), 1_000_000).await?;

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
                amount: Some(100_000),
                bolt12: None,
            },
        )
        .await?;
    log::info!(target: &node1.info.node_id, "payment made `{:?}`", pay);
    assert_eq!(pay.state, response::PaymentState::Success);
    assert!(pay.payment_hash.is_some(), "Payment hash should be present");
    assert_eq!(
        pay.path.last().unwrap().node_id,
        node2.info.node_id,
        "Last hop should be to the destination node"
    );
    Ok(())
}

#[tokio_test_shutdown_timeout::test(10)]
pub async fn decode_invoice() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let btc = node1.btc.clone();
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);

    node1.fund_channel_with(node2.clone(), 1_000_000).await?;

    let invoice: response::Invoice = node2
        .lampod()
        .call(
            "invoice",
            request::GenerateInvoice {
                description: "test decode".to_owned(),
                amount_msat: Some(100_000),
                expiring_in: None,
            },
        )
        .await?;

    log::info!(target: &node2.info.node_id, "invoice generated `{:?}`", invoice);

    let decode_result: response::Decode = node2
        .lampod()
        .call(
            "decode",
            request::DecodeInvoice {
                invoice_str: invoice.bolt11.clone(),
            },
        )
        .await?;

    let decode: response::Bolt11InvoiceInfo = match decode_result {
        response::Decode::Bolt11(x) => x,
        _ => panic!("Should be a bolt11 invoice"),
    };

    assert_eq!(decode.issuer_id.clone(), Some(node2.info.node_id.clone()));
    log::info!(target: &node2.info.node_id, "decode offer `{:?}`", decode);

    let pay: response::PayResult = node1
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: invoice.bolt11,
                amount: None,
                bolt12: None,
            },
        )
        .await?;
    log::info!(target: &node1.info.node_id, "Payment call result from node1: {:?}", pay);

    assert_eq!(pay.state, response::PaymentState::Success);
    assert!(pay.payment_hash.is_some(), "Payment hash should be present");
    assert_eq!(
        pay.path.last().unwrap().node_id,
        node2.info.node_id,
        "Last hop should be to the destination node"
    );
    Ok(())
}

#[tokio_test_shutdown_timeout::test(10)]
pub async fn decode_offer_hex() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let btc = node1.btc.clone();
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);

    node1.fund_channel_with(node2.clone(), 1_000_000).await?;

    let offer: response::Offer = node2
        .lampod()
        .call(
            "offer",
            request::GenerateOffer {
                description: Some("test offer for decode".to_owned()),
                amount_msat: Some(100_000),
            },
        )
        .await?;

    log::info!(target: &node2.info.node_id, "offer generated `{:?}`", offer);

    let decode_result: response::Decode = node2
        .lampod()
        .call(
            "decode",
            request::DecodeInvoice {
                invoice_str: offer.bolt12.clone(),
            },
        )
        .await?;

    let decode: response::Bolt12InvoiceInfo = match decode_result {
        response::Decode::Bolt12(x) => x,
        _ => panic!("Should be a bolt12 invoice"),
    };

    assert!(!decode.offer_id.is_empty(), "Offer ID should be present");
    assert_eq!(decode.network, "regtest", "Network should be regtest");
    assert_eq!(
        decode.description,
        Some("test offer for decode".to_owned()),
        "Description should match"
    );

    log::info!(target: &node1.info.node_id, "Successfully decoded offer with ID: {}", decode.offer_id);

    let pay: response::PayResult = node1
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: offer.bolt12,
                amount: None,
                bolt12: None,
            },
        )
        .await?;

    assert_eq!(
        pay.state,
        response::PaymentState::Success,
        "Payment should succeed"
    );
    assert!(pay.payment_hash.is_some(), "Payment hash should be present");
    assert!(!pay.path.is_empty(), "Payment path should not be empty");
    assert_eq!(
        pay.path.last().unwrap().node_id,
        node2.info.node_id,
        "Last hop should be to the destination node"
    );

    log::info!(target: &node1.info.node_id, "Payment completed successfully: {:?}", pay);
    Ok(())
}
