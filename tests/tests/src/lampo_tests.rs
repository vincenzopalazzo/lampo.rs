//! Integration tests between lampo nodes.
//!
//! Author: Vincenzo Palazzo <vincenzopalazzo@member.fsf.org>
use std::str::FromStr;
use std::sync::Arc;

use lampo_common::hex;
use lampo_common::ldk::offers::payer_proof::PayerProof;

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
                push_msat: None,
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

/// A settled payment must show up in `listpayments`, from the store rather
/// than from memory, which is what makes the history survive a restart.
#[tokio_test_shutdown_timeout::test(5)]
pub async fn listpayments_reports_a_settled_payment() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let btc = node1.btc.clone();
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);
    let _: response::Connect = node2
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

    let _: json::Value = node1
        .lampod()
        .call(
            "fundchannel",
            request::OpenChannel {
                node_id: node2.info.node_id.clone(),
                amount: 100000,
                public: true,
                port: None,
                addr: None,
                push_msat: None,
            },
        )
        .await
        .unwrap();
    node2.fund_wallet(10).await.unwrap();

    async_wait!(async {
        let channels: response::Channels = node1
            .lampod()
            .call("channels", json::json!({}))
            .await
            .unwrap();
        if channels.channels.iter().any(|channel| channel.ready) {
            return Ok(());
        }
        Err(())
    });

    let invoice: response::Invoice = node2
        .lampod()
        .call(
            "invoice",
            request::GenerateInvoice {
                amount_msat: Some(1_000),
                description: "listpayments".to_owned(),
                expiring_in: None,
            },
        )
        .await
        .unwrap();
    let _: response::PayResult = node1
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: invoice.bolt11,
                amount: None,
                bolt12: None,
                timeout: Default::default(),
            },
        )
        .await
        .unwrap();

    // The payer records the payment it sent...
    async_wait!(async {
        let payments: response::ListPayments = node1
            .lampod()
            .call("listpayments", json::json!({}))
            .await
            .unwrap();
        if payments
            .payments
            .iter()
            .any(|payment| payment.direction == "outbound" && payment.status == "succeeded")
        {
            return Ok(());
        }
        Err(())
    });

    // ...and the payee records the one it received.
    async_wait!(async {
        let payments: response::ListPayments = node2
            .lampod()
            .call("listpayments", json::json!({}))
            .await
            .unwrap();
        if payments
            .payments
            .iter()
            .any(|payment| payment.direction == "inbound" && payment.amount_msat == 1_000)
        {
            return Ok(());
        }
        Err(())
    });

    // The filter has to reach the store rather than being ignored.
    let none: response::ListPayments = node1
        .lampod()
        .call("listpayments", json::json!({"status": "failed"}))
        .await
        .unwrap();
    assert!(none.payments.is_empty(), "{:?}", none.payments);
    Ok(())
}

/// A restarted node must find its channel state in the store, whichever
/// backend holds it.
///
/// This used to pass trivially on the filesystem and break silently on a
/// database backend: `is_restarting` checked for a literal `<root>/manager`
/// file, so a sqlite- or postgres-backed node came back as a brand-new node
/// while its monitors sat in the database. Run with
/// `LAMPO_TEST_STORAGE=sqlite` to exercise the database path.
#[tokio_test_shutdown_timeout::test(5)]
pub async fn channel_survives_a_node_restart() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let btc = node1.btc.clone();
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);
    let _: response::Connect = node2
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
                push_msat: None,
            },
        )
        .await
        .unwrap();
    assert!(response.get("tx").is_some());
    node2.fund_wallet(10).await.unwrap();

    async_wait!(async {
        let channels: response::Channels = node1
            .lampod()
            .call("channels", json::json!({}))
            .await
            .unwrap();
        if channels.channels.is_empty() {
            return Err(());
        }
        Ok(())
    });

    let node1 = node1.restart().await?;
    let channels: response::Channels = node1
        .lampod()
        .call("channels", json::json!({}))
        .await
        .unwrap();
    assert!(
        !channels.channels.is_empty(),
        "the restarted node lost its channel: state was not restored from the store"
    );
    Ok(())
}

/// `fundchannel` must honor the `public` flag it accepts.
///
/// This used to fail: `open_channel` parsed `public` into the request and
/// then handed LDK the global `ldk_conf`, whose `announce_for_forwarding`
/// is false by default. Every channel came up unannounced no matter what
/// the caller asked for, so gossip never learned the channel existed and
/// nothing could be routed *through* a lampo node -- while `fundchannel`
/// returned success and reported the channel as whatever was requested.
///
/// `public` on the channel list is LDK's own `ChannelDetails::is_announced`
/// ("true if this channel is (or will be) publicly-announced"), so this
/// asserts the negotiated state rather than echoing the request back.
#[tokio_test_shutdown_timeout::test(5)]
pub async fn fundchannel_honors_the_public_flag() -> error::Result<()> {
    init();

    // Both directions are pinned: asserting only the announced case would
    // still pass if the flag were hardcoded the other way.
    for announce in [true, false] {
        let node1 = LampoTesting::tmp().await?;
        let node2 = Arc::new(LampoTesting::new(node1.btc.clone()).await?);

        let _: response::Connect = node2
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

        let mut events = node2.lampod().events();
        let response: json::Value = node1
            .lampod()
            .call(
                "fundchannel",
                request::OpenChannel {
                    node_id: node2.info.node_id.clone(),
                    amount: 100000,
                    public: announce,
                    port: None,
                    addr: None,
                    push_msat: None,
                },
            )
            .await
            .unwrap();
        assert!(response.get("tx").is_some());
        node2.fund_wallet(10).await.unwrap();

        async_wait!(async {
            while let Some(event) = events.recv().await {
                if let Event::Lightning(LightningEvent::ChannelReady { .. }) = event {
                    return Ok(());
                }
                let channels: response::Channels = node1
                    .lampod()
                    .call("channels", json::json!({}))
                    .await
                    .unwrap();
                if channels.channels.iter().any(|chan| chan.ready) {
                    return Ok(());
                }
            }
            Err(())
        });

        // The opener is the side that chose the flag, so assert there.
        let channels: response::Channels = node1.lampod().call("channels", json::json!({})).await?;
        let channel = channels
            .channels
            .first()
            .expect("the channel we just opened should be listed");
        assert_eq!(
            channel.public, announce,
            "asked for public={announce}, got public={} -- the flag was ignored",
            channel.public
        );
    }
    Ok(())
}

/// A node must redial its channel counterparties on its own.
///
/// LDK never reconnects to anyone; without lampo's reconnect loop a node
/// that loses a TCP connection (or restarts) sits at zero peers forever
/// with a live channel -- it stops forwarding and cannot be paid. This
/// was observed on a real deployment: two nodes with a ready channel,
/// both restarted, both at `peers=0` indefinitely.
///
/// The loop dials from the persisted last-known address, so it works for
/// unannounced peers too -- which is what this test exercises, since the
/// nodes here have no announce address and the graph knows nothing.
#[tokio_test_shutdown_timeout::test(60)]
pub async fn channel_peer_reconnects_after_disconnect() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let node2 = Arc::new(LampoTesting::new(node1.btc.clone()).await?);
    node1.fund_channel_with(node2.clone(), 1_000_000).await?;

    let peer = lampo_common::types::NodeId::from_str(&node2.info.node_id)?;
    let peer_manager = node1.lampod().peer_manager();
    assert!(
        peer_manager.is_connected_with(peer),
        "funding a channel should leave the peers connected"
    );

    peer_manager.disconnect(peer).await?;
    assert!(
        !peer_manager.is_connected_with(peer),
        "disconnect should actually drop the connection"
    );

    // No RPC, no manual connect: the reconnect loop alone must bring the
    // channel peer back (it ticks every 10s).
    async_wait!(
        async {
            if node1.lampod().peer_manager().is_connected_with(peer) {
                Ok(())
            } else {
                Err(())
            }
        },
        5
    );
    Ok(())
}

#[tokio_test_shutdown_timeout::test(5)]
pub async fn pay_invoice_simple_case_lampo() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let btc = node1.btc.clone();
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);

    // There is a channel node1 -> node2
    node1.fund_channel_with(node2.clone(), 1_000_000).await?;

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
                timeout: Default::default(),
            },
        )
        .await?;
    log::info!(target: &node2.info.node_id, "payment made `{:?}`", pay);

    // BOLT 11 has no payer proof, but the preimage is still the receipt.
    assert!(
        pay.payment_preimage.is_some(),
        "a settled bolt11 payment must expose its preimage"
    );
    assert!(
        pay.payer_proof.is_none(),
        "bolt11 payments cannot produce a payer proof"
    );
    Ok(())
}

#[tokio_test_shutdown_timeout::test(10)]
pub async fn pay_offer_simple_case_lampo() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let btc = node1.btc.clone();
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);

    // There is a channel node1 -> node2
    node1.fund_channel_with(node2.clone(), 1_000_000).await?;

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
                invoice_str: offer.bolt12.clone(),
                amount: None,
                bolt12: None,
                timeout: Default::default(),
            },
        )
        .await?;
    log::info!(target: &node2.info.node_id, "payment made `{:?}`", pay);

    // Paying an offer must hand back a payer proof a third party can check
    // against the payment we actually made.
    let preimage = pay
        .payment_preimage
        .expect("a settled offer payment must expose its preimage");
    // There is no separate verify entry point: LDK runs the checks while
    // parsing, in `TryFrom<Vec<u8>> for PayerProof`. Parsing here is the
    // verification, and it covers preimage against payment hash, the invoice
    // signature against the issuer key, and the payer signature over the
    // merkle root.
    let proof = PayerProof::from_str(
        &pay.payer_proof
            .expect("a settled offer payment must expose a payer proof"),
    )
    .expect("the payer proof must verify");

    assert_eq!(
        proof.payment_hash().to_string(),
        pay.payment_hash
            .as_deref()
            .expect("a settled offer payment must expose its hash"),
        "the proof must commit to the hash of the payment we made"
    );
    assert_eq!(
        hex::encode(proof.payment_preimage().0),
        preimage,
        "the proof must carry the same preimage the RPC returned"
    );

    // Offers are reusable. A second payment must get its own payment id and
    // history row rather than replacing the first attempt.
    let second: response::PayResult = node1
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: offer.bolt12,
                amount: None,
                bolt12: None,
                timeout: Default::default(),
            },
        )
        .await?;
    assert_ne!(
        pay.payment_hash, second.payment_hash,
        "each offer payment must have a unique hash"
    );

    async_wait!(async {
        let payments: response::ListPayments = node1
            .lampod()
            .call(
                "listpayments",
                json::json!({"direction": "outbound", "status": "succeeded"}),
            )
            .await
            .unwrap();
        if payments.payments.len() == 2
            && payments.payments[0].id != payments.payments[1].id
            && payments.payments[0].payment_hash != payments.payments[1].payment_hash
            && payments
                .payments
                .iter()
                .all(|payment| payment.amount_msat == 100_000)
        {
            Ok(())
        } else {
            Err(())
        }
    });
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
                timeout: Default::default(),
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
                timeout: Default::default(),
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
                timeout: Default::default(),
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

/// After a channel closes, funds return to a wallet-owned destination
/// script (and delayed outputs through the sweeper). Without that wiring
/// this test times out with the wallet stuck at its post-funding value.
/// GHSA-pw22-mxxj-rvgh.
#[tokio_test_shutdown_timeout::test(10)]
pub async fn sweep_funds_after_channel_close() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let btc = node1.btc.clone();
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);

    const CHANNEL_SAT: u64 = 1_000_000;
    node1.fund_channel_with(node2.clone(), CHANNEL_SAT).await?;

    // The sweep lands as a fresh wallet UTXO worth the channel balance
    // minus close and sweep fees, i.e. somewhere in (CHANNEL_SAT / 2,
    // CHANNEL_SAT). Nothing else in this test produces an output in that
    // range: coinbases are ~50 BTC and the funding change is far larger.
    let in_sweep_range = |utxo: &response::Utxo| {
        let sat = utxo.amount_msat / 1000;
        sat > CHANNEL_SAT / 2 && sat < CHANNEL_SAT
    };
    let funds: response::Utxos = node1.lampod().call("funds", json::json!({})).await?;
    assert!(
        !funds.transactions.iter().any(in_sweep_range),
        "no sweep-sized utxo should exist before the close: {funds:?}"
    );

    let close: response::CloseChannel = node1
        .lampod()
        .call(
            "close",
            request::CloseChannel {
                node_id: node2.info.node_id.clone(),
                channel_id: None,
            },
        )
        .await?;
    log::info!(target: &node1.info.node_id, "channel closed: {close:?}");

    // The closing transaction needs to confirm (plus LDK's anti-reorg
    // delay of 6 blocks) before SpendableOutputs fires; the sweeper then
    // broadcasts on the background processor's 30s timer and the sweep
    // itself needs a confirmation. Keep mining and give it time.
    async_wait!(
        async {
            node1.fund_wallet(2).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let funds: response::Utxos =
                node1.lampod().call("funds", json::json!({})).await.unwrap();
            log::info!(
                target: &node1.info.node_id,
                "waiting for the sweep to land: {} utxos",
                funds.transactions.len()
            );
            if funds.transactions.iter().any(in_sweep_range) {
                Ok(())
            } else {
                Err(())
            }
        },
        10
    );
    Ok(())
}
