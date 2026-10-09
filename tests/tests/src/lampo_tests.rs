//! Integration tests between lampo nodes.
//!
//! Author: Vincenzo Palazzo <vincenzopalazzo@member.fsf.org>
use std::str::FromStr;
use std::sync::Arc;

use lampo_common::hex;
use lampo_common::ldk::offers::offer::Offer;
use lampo_common::ldk::offers::payer_proof::PayerProof;

use lampo_common::error;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json;
use lampo_common::model::{request, response};
use lampo_common::types::NodeId;

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
                sat_per_vbyte: None,
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
                if counterparty_node_id != NodeId::from_str(&node1.info.node_id).unwrap() {
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
                    push_msat: None,
                    sat_per_vbyte: None,
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
                max_fee_msat: None,
                bolt12: None,
                timeout: Default::default(),
                timeout_secs: None,
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
                currency: None,
                currency_amount: None,
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
                max_fee_msat: None,
                bolt12: None,
                timeout: Default::default(),
                timeout_secs: None,
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
                currency: None,
                currency_amount: None,
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
                max_fee_msat: None,
                bolt12: None,
                timeout: Default::default(),
                timeout_secs: None,
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
    assert_eq!(decode.payment_hash.len(), 64);
    assert!(decode.timestamp > 0);
    assert_eq!(decode.expiry_time, Some(10_000));
    log::info!(target: &node2.info.node_id, "decode offer `{:?}`", decode);

    let pay: response::PayResult = node1
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: invoice.bolt11,
                amount: None,
                max_fee_msat: None,
                bolt12: None,
                timeout: Default::default(),
                timeout_secs: None,
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
                currency: None,
                currency_amount: None,
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
                max_fee_msat: None,
                bolt12: None,
                timeout: Default::default(),
                timeout_secs: None,
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
                force: false,
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

/// Spin up a static-invoice server and a recipient, and return them once
/// their channel is usable.
///
/// The server -> recipient channel is **private**: a recipient that appears
/// in the network graph (via a public channel announcement) but announces no
/// addresses builds self-introduced blinded paths, and `DefaultMessageRouter`
/// cannot route onion messages to announced nodes without addresses. Keeping
/// the recipient out of the graph makes its paths introduction-point at the
/// server, which every payer of its offers is connected to by construction.
async fn async_payments_server_and_recipient(
    btc: Arc<BtcNode>,
) -> error::Result<(Arc<LampoTesting>, Arc<LampoTesting>)> {
    let server = Arc::new(
        LampoTesting::new_with(btc, |conf| {
            conf.async_payments_role = Some("server".to_owned());
        })
        .await?,
    );
    let recipient = Arc::new(LampoTesting::new(server.btc.clone()).await?);

    // server -> recipient: the recipient needs inbound liquidity for the
    // static invoice's payment paths, and the channel connects the two for
    // the onion-message handshake that builds the offer.
    server
        .fund_channel_with_privacy(recipient.clone(), 1_000_000, false)
        .await?;

    Ok((server, recipient))
}

/// Wait until `node`'s own graph knows `node_id`, i.e. a channel
/// announcement naming it has been processed. A node only mints
/// self-introduced blinded paths once it is announced; minting earlier
/// produces paths introduced by whatever peer happened to be connected.
async fn wait_node_in_graph(node: &LampoTesting, node_id: &str) {
    let node_id = lampo_common::bitcoin::secp256k1::PublicKey::from_str(node_id).unwrap();
    let node_id = lampo_common::ldk::routing::gossip::NodeId::from_pubkey(&node_id);
    async_wait!(
        async {
            let graph = node.daemon().channel_manager().graph();
            if graph.read_only().nodes().contains_key(&node_id) {
                Ok(())
            } else {
                Err(())
            }
        },
        5
    );
}

/// Mint server paths over RPC and install them on the recipient.
///
/// This is the operator path: `asyncinvoicepaths` on the server, then
/// `setasyncinvoicepaths` on the recipient (or the hex in
/// `async-invoice-server-paths`).
async fn provision_async_receive(
    server: &LampoTesting,
    recipient: &LampoTesting,
) -> error::Result<String> {
    let paths: response::AsyncInvoicePaths = server
        .lampod()
        .call(
            "asyncinvoicepaths",
            request::GenerateAsyncInvoicePaths {
                node_id: recipient.info.node_id.clone(),
                token: None,
            },
        )
        .await?;
    assert!(
        !paths.paths.is_empty(),
        "server must return hex-encoded blinded paths"
    );
    recipient
        .lampod()
        .call::<_, response::AsyncInvoicePaths>(
            "setasyncinvoicepaths",
            request::SetAsyncInvoicePaths {
                paths: paths.paths.clone(),
                token: None,
                force: false,
            },
        )
        .await?;
    Ok(paths.paths)
}

/// Wait for the recipient's async receive offer to be built with the server.
async fn wait_async_offer(node: &LampoTesting) -> response::Offer {
    let mut offer = None;
    async_wait!(
        async {
            match node
                .lampod()
                .call::<_, response::Offer>(
                    "offer",
                    request::GenerateOffer {
                        description: None,
                        amount_msat: None,
                        currency: None,
                        currency_amount: None,
                    },
                )
                .await
            {
                Ok(o) => {
                    offer = Some(o);
                    Ok(())
                }
                Err(err) => {
                    log::debug!(target: "tests", "async offer not ready yet: {err}");
                    Err(())
                }
            }
        },
        5
    );
    offer.unwrap()
}

#[tokio_test_shutdown_timeout::test(120)]
pub async fn async_receive_offer_roundtrip() -> error::Result<()> {
    init();
    let server = LampoTesting::tmp_with(|conf| {
        conf.async_payments_role = Some("server".to_owned());
    })
    .await?;
    let recipient = Arc::new(LampoTesting::new(server.btc.clone()).await?);

    server
        .fund_channel_with(recipient.clone(), 1_000_000)
        .await?;

    // Minting before the server's own graph has processed its channel
    // announcement would produce paths introduced by the recipient instead
    // of self-introduced ones — fine for this two-node handshake, but not
    // the shape a real payer relies on.
    wait_node_in_graph(&server, &server.info.node_id.clone()).await;

    // A non-server node cannot mint paths.
    let not_server = recipient
        .lampod()
        .call::<_, response::AsyncInvoicePaths>(
            "asyncinvoicepaths",
            request::GenerateAsyncInvoicePaths {
                node_id: recipient.info.node_id.clone(),
                token: None,
            },
        )
        .await;
    assert!(
        not_server.is_err(),
        "asyncinvoicepaths is server-role only, got {not_server:?}"
    );

    // Minting is bound to a channel counterparty, not an arbitrary id.
    let not_peer = server
        .lampod()
        .call::<_, response::AsyncInvoicePaths>(
            "asyncinvoicepaths",
            request::GenerateAsyncInvoicePaths {
                node_id: server.info.node_id.clone(),
                token: None,
            },
        )
        .await;
    assert!(
        not_peer.is_err(),
        "asyncinvoicepaths requires a channel counterparty, got {not_peer:?}"
    );

    let paths = provision_async_receive(&server, &recipient).await?;
    let on_server = server
        .lampod()
        .call::<_, response::AsyncInvoicePaths>(
            "setasyncinvoicepaths",
            request::SetAsyncInvoicePaths {
                paths: paths.clone(),
                token: None,
                force: false,
            },
        )
        .await;
    assert!(
        on_server.is_err(),
        "setasyncinvoicepaths is not for the server role, got {on_server:?}"
    );
    let overwrite = recipient
        .lampod()
        .call::<_, response::AsyncInvoicePaths>(
            "setasyncinvoicepaths",
            request::SetAsyncInvoicePaths {
                paths: paths.clone(),
                token: None,
                force: false,
            },
        )
        .await;
    assert!(
        overwrite.is_err(),
        "setasyncinvoicepaths must not overwrite without force, got {overwrite:?}"
    );
    recipient
        .lampod()
        .call::<_, response::AsyncInvoicePaths>(
            "setasyncinvoicepaths",
            request::SetAsyncInvoicePaths {
                paths,
                token: None,
                force: true,
            },
        )
        .await?;

    let offer = wait_async_offer(&recipient).await;
    assert!(
        offer.bolt12.starts_with("lno1"),
        "expected a BOLT12 offer, got `{}`",
        offer.bolt12
    );
    Ok(())
}

#[tokio_test_shutdown_timeout::test(180)]
pub async fn async_payment_held_htlc_roundtrip() -> error::Result<()> {
    init();
    // The payer is a private node that asks its next hop to hold the HTLC,
    // so the server exercises the full hold/release cycle.
    let payer = LampoTesting::tmp_with(|conf| {
        conf.async_payments_role = Some("client".to_owned());
    })
    .await?;
    let (server, recipient) = async_payments_server_and_recipient(payer.btc.clone()).await?;

    // payer -> server, so the payer can reach the static invoice's paths.
    // Public: the server must be announced before it mints self-introduced
    // paths for the recipient.
    payer.fund_channel_with(server.clone(), 1_000_000).await?;
    wait_node_in_graph(&server, &server.info.node_id.clone()).await;

    provision_async_receive(&server, &recipient).await?;

    let offer = wait_async_offer(&recipient).await;
    log::info!(target: &payer.info.node_id, "paying async offer `{}`", offer.bolt12);

    let pay: response::PayResult = payer
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: offer.bolt12,
                amount: Some(100_000),
                bolt12: None,
                timeout: Default::default(),
                max_fee_msat: None,
                timeout_secs: None,
            },
        )
        .await?;
    log::info!(target: &recipient.info.node_id, "async payment made `{pay:?}`");

    assert_eq!(pay.state, response::PaymentState::Success);
    assert!(
        pay.payment_preimage.is_some(),
        "a settled async payment must expose its preimage"
    );
    // The payment settled against the static invoice served by the server,
    // not a live invoice from the recipient: static invoices cannot produce
    // a payer proof, while a live BOLT12 payment always does here. The hold
    // itself is not directly observable from the test; it was verified during
    // development via the server's `Intercepted held HTLC ... holding until
    // the recipient is online` log line.
    assert!(
        pay.payer_proof.is_none(),
        "a static-invoice payment cannot carry a payer proof"
    );
    Ok(())
}

/// Same topology as [`async_payment_held_htlc_roundtrip`], but the
/// recipient is taken off the reconnect loop. A pay while they are down
/// must buffer onion messages in the mailbox (the HTLC itself fails),
/// an explicit reconnect must drain that mailbox, and a later pay must
/// still settle.
#[tokio_test_shutdown_timeout::test(180)]
pub async fn async_payment_offline_recipient_roundtrip() -> error::Result<()> {
    init();
    let payer = LampoTesting::tmp_with(|conf| {
        conf.async_payments_role = Some("client".to_owned());
    })
    .await?;
    let (server, recipient) = async_payments_server_and_recipient(payer.btc.clone()).await?;
    payer.fund_channel_with(server.clone(), 1_000_000).await?;
    wait_node_in_graph(&server, &server.info.node_id.clone()).await;

    provision_async_receive(&server, &recipient).await?;
    let offer = wait_async_offer(&recipient).await;

    let recipient_id = lampo_common::types::NodeId::from_str(&recipient.info.node_id)?;
    let server_id = lampo_common::types::NodeId::from_str(&server.info.node_id)?;
    // Both sides redial channel counterparties from peers.json. Forget
    // first so an in-flight tick cannot reload the address after we
    // disconnect.
    server.lampod().peer_manager().forget_peer(&recipient_id);
    recipient.lampod().peer_manager().forget_peer(&server_id);
    server
        .lampod()
        .peer_manager()
        .disconnect(recipient_id)
        .await?;
    if recipient
        .lampod()
        .peer_manager()
        .is_connected_with(server_id)
    {
        recipient
            .lampod()
            .peer_manager()
            .disconnect(server_id)
            .await?;
    }
    for _ in 0..20 {
        if server
            .lampod()
            .peer_manager()
            .is_connected_with(recipient_id)
        {
            server.lampod().peer_manager().forget_peer(&recipient_id);
            recipient.lampod().peer_manager().forget_peer(&server_id);
            let _ = server
                .lampod()
                .peer_manager()
                .disconnect(recipient_id)
                .await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        !server
            .lampod()
            .peer_manager()
            .is_connected_with(recipient_id),
        "recipient must stay offline; the reconnect loop must not win"
    );

    // A client payer sends HeldHtlcAvailable (and the HTLC) immediately.
    // The HTLC fails while the recipient is down; the onion message must
    // still be buffered. After reconnect the mailbox drains, and a second
    // pay settles on the live path.
    let payer_rpc = payer.lampod();
    let offer_str = offer.bolt12.clone();
    let offline_pay = tokio::spawn(async move {
        payer_rpc
            .call::<_, response::PayResult>(
                "pay",
                request::Pay {
                    invoice_str: offer_str,
                    amount: Some(100_000),
                    bolt12: None,
                    timeout: request::PayTimeout::Fast,
                    max_fee_msat: None,
                    timeout_secs: None,
                },
            )
            .await
    });

    async_wait!(
        async {
            if server.lampod().buffered_onion_messages(recipient_id) > 0 {
                Ok(())
            } else {
                Err(())
            }
        },
        5
    );
    assert!(
        !server
            .lampod()
            .peer_manager()
            .is_connected_with(recipient_id),
        "mailbox must fill while the recipient is still offline"
    );

    recipient
        .lampod()
        .call::<_, response::Connect>(
            "connect",
            request::Connect {
                node_id: server.info.node_id.clone(),
                addr: "127.0.0.1".to_owned(),
                port: server.port,
            },
        )
        .await?;
    async_wait!(
        async {
            if server.lampod().buffered_onion_messages(recipient_id) == 0
                && server
                    .lampod()
                    .peer_manager()
                    .is_connected_with(recipient_id)
            {
                Ok(())
            } else {
                Err(())
            }
        },
        5
    );
    let _ = offline_pay.await;

    let pay: response::PayResult = payer
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: offer.bolt12,
                amount: Some(100_000),
                bolt12: None,
                timeout: Default::default(),
                max_fee_msat: None,
                timeout_secs: None,
            },
        )
        .await?;
    assert_eq!(pay.state, response::PaymentState::Success);
    assert!(
        pay.payment_preimage.is_some(),
        "a settled async payment must expose its preimage"
    );
    assert!(
        pay.payer_proof.is_none(),
        "a static-invoice payment cannot carry a payer proof"
    );
    Ok(())
}

#[tokio_test_shutdown_timeout::test(120)]
pub async fn asyncinvoicepaths_requires_token_when_configured() -> error::Result<()> {
    init();
    let server = LampoTesting::tmp_with(|conf| {
        conf.async_payments_role = Some("server".to_owned());
        conf.api_token = Some("server-secret".to_owned());
    })
    .await?;
    let recipient = Arc::new(
        LampoTesting::new_with(server.btc.clone(), |conf| {
            conf.api_token = Some("recipient-secret".to_owned());
        })
        .await?,
    );
    server
        .fund_channel_with(recipient.clone(), 1_000_000)
        .await?;
    wait_node_in_graph(&server, &server.info.node_id.clone()).await;

    let missing = server
        .lampod()
        .call::<_, response::AsyncInvoicePaths>(
            "asyncinvoicepaths",
            request::GenerateAsyncInvoicePaths {
                node_id: recipient.info.node_id.clone(),
                token: None,
            },
        )
        .await;
    assert!(
        missing.is_err(),
        "asyncinvoicepaths must require the configured token, got {missing:?}"
    );
    let wrong = server
        .lampod()
        .call::<_, response::AsyncInvoicePaths>(
            "asyncinvoicepaths",
            request::GenerateAsyncInvoicePaths {
                node_id: recipient.info.node_id.clone(),
                token: Some("nope".to_owned()),
            },
        )
        .await;
    assert!(
        wrong.is_err(),
        "asyncinvoicepaths must reject the wrong token, got {wrong:?}"
    );
    let paths: response::AsyncInvoicePaths = server
        .lampod()
        .call(
            "asyncinvoicepaths",
            request::GenerateAsyncInvoicePaths {
                node_id: recipient.info.node_id.clone(),
                token: Some("server-secret".to_owned()),
            },
        )
        .await?;

    let set_missing = recipient
        .lampod()
        .call::<_, response::AsyncInvoicePaths>(
            "setasyncinvoicepaths",
            request::SetAsyncInvoicePaths {
                paths: paths.paths.clone(),
                token: None,
                force: false,
            },
        )
        .await;
    assert!(
        set_missing.is_err(),
        "setasyncinvoicepaths must require the configured token, got {set_missing:?}"
    );
    recipient
        .lampod()
        .call::<_, response::AsyncInvoicePaths>(
            "setasyncinvoicepaths",
            request::SetAsyncInvoicePaths {
                paths: paths.paths,
                token: Some("recipient-secret".to_owned()),
                force: false,
            },
        )
        .await?;
    Ok(())
}

#[tokio_test_shutdown_timeout::test(60)]
pub async fn pay_offer_blip42_contact_roundtrip() -> error::Result<()> {
    init();
    let node1 = LampoTesting::tmp().await?;
    let btc = node1.btc.clone();
    let node2 = Arc::new(LampoTesting::new(btc.clone()).await?);
    // Push half the capacity so node2 can pay back on the same channel
    // (mirrors LDK's announced-chan-with-push setup for BLIP-42 tests).
    node1
        .fund_channel_with_push(node2.clone(), 1_000_000, Some(500_000_000))
        .await?;

    let offer: response::Offer = node2
        .lampod()
        .call(
            "offer",
            request::GenerateOffer {
                description: Some("blip42 contact offer".to_owned()),
                amount_msat: Some(50_000),
                currency: None,
                currency_amount: None,
            },
        )
        .await?;

    // node1 pays node2 and reveals BLIP-42 contact info using node2 as intro peer.
    let pay: response::PayResult = node1
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: offer.bolt12,
                amount: None,
                bolt12: Some(request::Bolt12Pay {
                    payer_note: Some("hi from node1".to_owned()),
                    reveal_contact: Some(true),
                    contact_label: Some("node2".to_owned()),
                    intro_node: Some(node2.info.node_id.clone()),
                }),
                timeout: Default::default(),
                max_fee_msat: None,
                timeout_secs: None,
            },
        )
        .await?;
    assert_eq!(
        pay.state,
        response::PaymentState::Success,
        "reveal-contact pay must succeed: {pay:?}"
    );
    assert!(pay.payment_preimage.is_some());

    // node1 should have stored the outbound contact.
    let n1_contacts: response::Contacts = node1
        .lampod()
        .call("listcontacts", request::ListContacts {})
        .await?;
    assert!(
        n1_contacts
            .contacts
            .iter()
            .any(|c| c.label == "node2" && c.our_offer.is_some()),
        "node1 must persist outbound contact with our_offer: {n1_contacts:?}"
    );

    // node2 should have remembered the inbound contact (+ payer offer return path).
    let n2_contacts: response::Contacts = node2
        .lampod()
        .call("listcontacts", request::ListContacts {})
        .await?;
    let inbound = n2_contacts
        .contacts
        .iter()
        .find(|c| !c.remote_offer.is_empty())
        .cloned()
        .expect(&format!(
            "node2 must store inbound BLIP-42 contact: {n2_contacts:?}"
        ));

    // Pay back using the stored compact payer offer + shared contact secret.
    let payback: response::PayResult = node2
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: inbound.remote_offer.clone(),
                amount: Some(40_000),
                bolt12: Some(request::Bolt12Pay {
                    payer_note: None,
                    reveal_contact: Some(false),
                    contact_label: Some(inbound.label.clone()),
                    intro_node: Some(node1.info.node_id.clone()),
                }),
                timeout: Default::default(),
                max_fee_msat: None,
                timeout_secs: None,
            },
        )
        .await?;
    assert_eq!(
        payback.state,
        response::PaymentState::Success,
        "payback via stored contact must succeed: {payback:?}"
    );
    assert!(payback.payment_preimage.is_some());
    Ok(())
}

/// Matt Corallo on rust-lightning #5008: a contact secret derived from one
/// static offer signing key lets Bob and Charlie compare notes and discover
/// Alice is the same person.
///
/// Each reveal must mint a fresh offer key (LDK's random-per-offer nonce) and
/// a secret that does not match an ECDH of the other contact's key. Repeating
/// a reveal to the same contact must keep that key and secret: a new ephemeral
/// secret on every pay is the other half of the review.
#[tokio_test_shutdown_timeout::test(90)]
pub async fn blip42_contact_keys_are_not_linkable_across_contacts() -> error::Result<()> {
    init();
    let alice = LampoTesting::tmp().await?;
    let btc = alice.btc.clone();
    let bob = Arc::new(LampoTesting::new(btc.clone()).await?);
    let charlie = Arc::new(LampoTesting::new(btc.clone()).await?);
    // Push so each payee can later pay back on the same channel. The intro
    // node is Alice's own peer; a shared intro is not the leak under review.
    alice
        .fund_channel_with_push(bob.clone(), 1_000_000, Some(500_000_000))
        .await?;
    alice
        .fund_channel_with_push(charlie.clone(), 1_000_000, Some(500_000_000))
        .await?;

    let bob_offer = offer_from(&bob, "bob contact offer").await?;
    let charlie_offer = offer_from(&charlie, "charlie contact offer").await?;

    pay_revealing(&alice, &bob_offer, "bob", &bob.info.node_id).await?;
    pay_revealing(&alice, &charlie_offer, "charlie", &charlie.info.node_id).await?;

    let alice_book = list_contacts(&alice).await?;
    let bob_contact = contact_labeled(&alice_book, "bob");
    let charlie_contact = contact_labeled(&alice_book, "charlie");
    assert_ne!(
        bob_contact.primary_secret_hex, charlie_contact.primary_secret_hex,
        "one static signing key would still be comparable across contacts: {alice_book:?}"
    );
    let bob_key = offer_signing_key(bob_contact.our_offer.as_deref().unwrap());
    let charlie_key = offer_signing_key(charlie_contact.our_offer.as_deref().unwrap());
    assert_ne!(
        bob_key, charlie_key,
        "Alice revealed one signing pubkey to Bob and Charlie"
    );

    // What Bob and Charlie can compare is the payer offer they stored, not
    // Alice's book. Recomputing Bob's key against Charlie's offer must not
    // yield Charlie's secret: that match is the static-key attack.
    let bob_inbound = inbound_contact(&bob).await?;
    let charlie_inbound = inbound_contact(&charlie).await?;
    assert_ne!(
        bob_inbound.primary_secret_hex, charlie_inbound.primary_secret_hex,
        "receivers saw the same contact secret"
    );
    assert_ne!(
        offer_signing_key(&bob_inbound.remote_offer),
        offer_signing_key(&charlie_inbound.remote_offer),
        "receivers saw the same payer-offer signing key"
    );
    assert_eq!(
        bob_inbound.primary_secret_hex,
        bob_contact.primary_secret_hex
    );
    assert_eq!(
        charlie_inbound.primary_secret_hex,
        charlie_contact.primary_secret_hex
    );

    // Same contact, second pay: key and secret stay put.
    pay_revealing(&alice, &bob_offer, "bob", &bob.info.node_id).await?;
    let again = contact_labeled(&list_contacts(&alice).await?, "bob");
    assert_eq!(again.our_offer, bob_contact.our_offer);
    assert_eq!(again.primary_secret_hex, bob_contact.primary_secret_hex);
    let bob_after = inbound_contact(&bob).await?;
    assert_eq!(bob_after.primary_secret_hex, bob_inbound.primary_secret_hex);
    assert_eq!(bob_after.remote_offer, bob_inbound.remote_offer);
    Ok(())
}

async fn offer_from(node: &LampoTesting, description: &str) -> error::Result<String> {
    let offer: response::Offer = node
        .lampod()
        .call(
            "offer",
            request::GenerateOffer {
                description: Some(description.to_owned()),
                amount_msat: Some(50_000),
                currency: None,
                currency_amount: None,
            },
        )
        .await?;
    Ok(offer.bolt12)
}

async fn pay_revealing(
    payer: &LampoTesting,
    offer: &str,
    label: &str,
    intro_node: &str,
) -> error::Result<()> {
    let pay: response::PayResult = payer
        .lampod()
        .call(
            "pay",
            request::Pay {
                invoice_str: offer.to_owned(),
                amount: None,
                bolt12: Some(request::Bolt12Pay {
                    payer_note: Some(format!("hi {label}")),
                    reveal_contact: Some(true),
                    contact_label: Some(label.to_owned()),
                    intro_node: Some(intro_node.to_owned()),
                }),
                timeout: Default::default(),
                max_fee_msat: None,
                timeout_secs: None,
            },
        )
        .await?;
    if pay.state != response::PaymentState::Success {
        error::bail!("reveal-contact pay to {label} failed: {pay:?}");
    }
    Ok(())
}

async fn list_contacts(node: &LampoTesting) -> error::Result<response::Contacts> {
    Ok(node
        .lampod()
        .call("listcontacts", request::ListContacts {})
        .await?)
}

fn contact_labeled(book: &response::Contacts, label: &str) -> response::ContactInfo {
    book.contacts
        .iter()
        .find(|c| c.label == label)
        .cloned()
        .unwrap_or_else(|| panic!("missing contact `{label}`: {book:?}"))
}

async fn inbound_contact(node: &LampoTesting) -> error::Result<response::ContactInfo> {
    let book = list_contacts(node).await?;
    let found = book
        .contacts
        .iter()
        .find(|c| !c.remote_offer.is_empty())
        .cloned();
    found.ok_or_else(|| error::anyhow!("no inbound BLIP-42 contact: {book:?}"))
}

fn offer_signing_key(offer: &str) -> String {
    let parsed = Offer::from_str(offer).unwrap_or_else(|err| panic!("invalid offer: {err:?}"));
    parsed
        .issuer_signing_pubkey()
        .map(|key| key.to_string())
        .unwrap_or_else(|| panic!("payer offer has no signing pubkey: {offer}"))
}
