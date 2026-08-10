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

#[tokio_test_shutdown_timeout::test(5)]
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
                invoice_str: offer.bolt12,
                amount: None,
                bolt12: None,
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
        pay.payment_hash.unwrap(),
        "the proof must commit to the hash of the payment we made"
    );
    assert_eq!(
        hex::encode(proof.payment_preimage().0),
        preimage,
        "the proof must carry the same preimage the RPC returned"
    );
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

/// Concurrent payments share the same event stream, so each caller
/// must get back the result of the payment it actually asked for.
#[tokio_test_shutdown_timeout::test(10)]
pub async fn concurrent_payments_do_not_cross_results() -> error::Result<()> {
    use std::str::FromStr;

    init();
    let node1 = LampoTesting::tmp().await?;
    let node2 = Arc::new(LampoTesting::new(node1.btc.clone()).await?);
    node1.fund_channel_with(node2.clone(), 1_000_000).await?;

    let mut invoices = Vec::new();
    for (i, amount) in [30_000u64, 70_000u64].iter().enumerate() {
        let invoice: response::Invoice = node2
            .lampod()
            .call(
                "invoice",
                request::GenerateInvoice {
                    description: format!("concurrent {i}"),
                    amount_msat: Some(*amount),
                    expiring_in: None,
                },
            )
            .await?;
        let hash = lampo_common::ldk::invoice::Bolt11Invoice::from_str(&invoice.bolt11)
            .map_err(|err| error::anyhow!("{err:?}"))?
            .payment_hash()
            .to_string();
        invoices.push((invoice.bolt11, hash));
    }

    let tasks: Vec<_> = invoices
        .iter()
        .map(|(bolt11, hash)| {
            let payer = node1.lampod().clone();
            let bolt11 = bolt11.clone();
            let hash = hash.clone();
            tokio::spawn(async move {
                let pay: error::Result<response::PayResult> = payer
                    .call(
                        "pay",
                        request::Pay {
                            invoice_str: bolt11,
                            amount: None,
                            bolt12: None,
                        },
                    )
                    .await;
                (hash, pay)
            })
        })
        .collect();

    for task in tasks {
        let (expected_hash, pay) = task.await?;
        let pay = pay?;
        assert_eq!(
            pay.payment_hash,
            Some(expected_hash),
            "each caller must receive the result of its own payment"
        );
    }
    Ok(())
}

#[tokio_test_shutdown_timeout::test(10)]
pub async fn fetchinvoice_then_payfetched() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let node2 = Arc::new(LampoTesting::new(node1.btc.clone()).await?);
    node1.fund_channel_with(node2.clone(), 1_000_000).await?;

    let offer: response::Offer = node2
        .lampod()
        .call(
            "offer",
            request::GenerateOffer {
                description: Some("fetch me".to_owned()),
                amount_msat: Some(70_000),
            },
        )
        .await?;

    // fetch the invoice without paying it
    let fetched: response::FetchInvoiceResult = node1
        .lampod()
        .call(
            "fetchinvoice",
            request::FetchInvoice {
                offer_str: offer.bolt12,
                amount_msat: None,
                payer_note: None,
                max_cltv_expiry_delta: None,
            },
        )
        .await?;
    assert_eq!(fetched.amount_msat, 70_000);
    assert_eq!(fetched.payment_hash.len(), 64);

    // nothing has been paid yet: this is the whole point of the fetch
    let pay: response::PayResult = node1
        .lampod()
        .call(
            "payfetched",
            request::PayFetched {
                payment_id: fetched.payment_id,
            },
        )
        .await?;
    assert_eq!(pay.state, response::PaymentState::Success);
    assert_eq!(pay.payment_hash, Some(fetched.payment_hash.clone()));
    // the preimage must be the proof of payment for the fetched hash
    use lampo_common::bitcoin::hashes::{sha256, Hash};
    use lampo_common::bitcoin::hex::FromHex;
    let preimage = pay.payment_preimage.expect("preimage on success");
    let preimage = Vec::<u8>::from_hex(&preimage)?;
    assert_eq!(
        sha256::Hash::hash(&preimage).to_string(),
        fetched.payment_hash
    );
    Ok(())
}

#[tokio_test_shutdown_timeout::test(10)]
pub async fn fetchinvoice_cancel_prevents_payment() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let node2 = Arc::new(LampoTesting::new(node1.btc.clone()).await?);
    node1.fund_channel_with(node2.clone(), 1_000_000).await?;

    let offer: response::Offer = node2
        .lampod()
        .call(
            "offer",
            request::GenerateOffer {
                description: Some("fetch and cancel".to_owned()),
                amount_msat: Some(70_000),
            },
        )
        .await?;

    let fetched: response::FetchInvoiceResult = node1
        .lampod()
        .call(
            "fetchinvoice",
            request::FetchInvoice {
                offer_str: offer.bolt12,
                amount_msat: None,
                payer_note: None,
                max_cltv_expiry_delta: None,
            },
        )
        .await?;

    let _: response::CancelFetchedResult = node1
        .lampod()
        .call(
            "cancelfetched",
            request::CancelFetched {
                payment_id: fetched.payment_id.clone(),
            },
        )
        .await?;

    // the invoice is gone, paying it must fail
    let pay: error::Result<response::PayResult> = node1
        .lampod()
        .call(
            "payfetched",
            request::PayFetched {
                payment_id: fetched.payment_id,
            },
        )
        .await;
    assert!(pay.is_err(), "paying a cancelled fetch must fail");
    Ok(())
}

/// Build a (preimage, payment_hash) pair for the hold invoice tests.
fn hold_preimage(seed: u8) -> (String, String) {
    use lampo_common::bitcoin::hashes::{sha256, Hash};
    let preimage = [seed; 32];
    let hash = sha256::Hash::hash(&preimage);
    (
        preimage.iter().map(|b| format!("{b:02x}")).collect(),
        hash.to_string(),
    )
}

#[tokio_test_shutdown_timeout::test(10)]
pub async fn hold_invoice_claim_settles_payment() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let node2 = Arc::new(LampoTesting::new(node1.btc.clone()).await?);
    node1.fund_channel_with(node2.clone(), 1_000_000).await?;

    let (preimage, payment_hash) = hold_preimage(7);
    let invoice: response::HoldInvoiceResult = node2
        .lampod()
        .call(
            "holdinvoice",
            request::HoldInvoice {
                payment_hash: payment_hash.clone(),
                amount_msat: Some(50_000),
                description: "hold me".to_owned(),
                expiring_in: None,
                min_final_cltv_expiry_delta: Some(144),
            },
        )
        .await?;
    assert_eq!(invoice.payment_hash, payment_hash);

    // subscribe before paying so the `PaymentHeld` event cannot be missed
    let mut events = node2.lampod().events();
    let payer = node1.lampod().clone();
    let pay_task = tokio::spawn(async move {
        payer
            .call::<request::Pay, response::PayResult>(
                "pay",
                request::Pay {
                    invoice_str: invoice.bolt11,
                    amount: None,
                    bolt12: None,
                },
            )
            .await
    });

    // wait for the payment to be held on the receiver side
    let held = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let event = events.recv().await.expect("event channel closed");
            if let Event::Lightning(LightningEvent::PaymentHeld {
                payment_hash: hash,
                amount_msat,
                ..
            }) = event
            {
                return (hash, amount_msat);
            }
        }
    })
    .await?;
    assert_eq!(held.0, payment_hash);
    assert_eq!(held.1, 50_000);

    let holds: response::ListHoldsResult =
        node2.lampod().call("listholds", json::json!({})).await?;
    assert_eq!(holds.holds.len(), 1);
    assert_eq!(holds.holds[0].status, response::HoldStatus::Held);

    let claim: response::HoldClaimResult = node2
        .lampod()
        .call(
            "holdclaim",
            request::HoldClaim {
                payment_preimage: preimage.clone(),
            },
        )
        .await?;
    assert_eq!(claim.payment_hash, payment_hash);

    let pay = pay_task.await??;
    assert_eq!(pay.state, response::PaymentState::Success);
    assert_eq!(pay.payment_preimage, Some(preimage));

    // the hold record is gone once the payment settles
    let holds: response::ListHoldsResult =
        node2.lampod().call("listholds", json::json!({})).await?;
    assert!(holds.holds.is_empty());
    Ok(())
}

#[tokio_test_shutdown_timeout::test(10)]
pub async fn hold_invoice_fail_rejects_payment() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let node2 = Arc::new(LampoTesting::new(node1.btc.clone()).await?);
    node1.fund_channel_with(node2.clone(), 1_000_000).await?;

    let (_, payment_hash) = hold_preimage(9);
    let invoice: response::HoldInvoiceResult = node2
        .lampod()
        .call(
            "holdinvoice",
            request::HoldInvoice {
                payment_hash: payment_hash.clone(),
                amount_msat: Some(50_000),
                description: "hold and fail".to_owned(),
                expiring_in: None,
                min_final_cltv_expiry_delta: Some(144),
            },
        )
        .await?;

    let mut events = node2.lampod().events();
    let payer = node1.lampod().clone();
    let pay_task = tokio::spawn(async move {
        payer
            .call::<request::Pay, response::PayResult>(
                "pay",
                request::Pay {
                    invoice_str: invoice.bolt11,
                    amount: None,
                    bolt12: None,
                },
            )
            .await
    });

    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let event = events.recv().await.expect("event channel closed");
            if let Event::Lightning(LightningEvent::PaymentHeld { .. }) = event {
                return;
            }
        }
    })
    .await?;

    let _: response::HoldFailResult = node2
        .lampod()
        .call(
            "holdfail",
            request::HoldFail {
                payment_hash: payment_hash.clone(),
            },
        )
        .await?;

    let pay = pay_task.await??;
    assert_eq!(pay.state, response::PaymentState::Failure);

    let holds: response::ListHoldsResult =
        node2.lampod().call("listholds", json::json!({})).await?;
    assert!(holds.holds.is_empty());
    Ok(())
}

/// Regression test: a payment to an invoice with an external payment
/// hash that is not registered as a hold must fail cleanly instead of
/// killing the receiving node event loop.
#[tokio_test_shutdown_timeout::test(10)]
pub async fn pay_external_hash_without_hold_fails_cleanly() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let node2 = Arc::new(LampoTesting::new(node1.btc.clone()).await?);
    node1.fund_channel_with(node2.clone(), 1_000_000).await?;

    let (_, payment_hash) = hold_preimage(3);
    let invoice: response::HoldInvoiceResult = node2
        .lampod()
        .call(
            "holdinvoice",
            request::HoldInvoice {
                payment_hash: payment_hash.clone(),
                amount_msat: Some(50_000),
                description: "no hold".to_owned(),
                expiring_in: None,
                min_final_cltv_expiry_delta: Some(144),
            },
        )
        .await?;
    // drop the hold registration: the invoice is still payable, but the
    // receiver has no record for it anymore
    let _: response::HoldFailResult = node2
        .lampod()
        .call("holdfail", request::HoldFail { payment_hash })
        .await?;

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
    assert_eq!(pay.state, response::PaymentState::Failure);

    // the receiving node must still be alive and answering
    let info: response::GetInfo = node2.lampod().call("getinfo", json::json!({})).await?;
    assert_eq!(info.node_id, node2.info.node_id);
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
