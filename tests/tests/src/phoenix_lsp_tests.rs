//! Two Lampo nodes where node `lsp` plays the ACINQ Phoenix LSP for node
//! `client`: the client dials it at startup, advertises the Phoenix feature
//! bits (155, 561 and 563, never 129) to it alone, shows what the LSP sends in
//! `phoenixlsp-info`, and
//! completes a `dns_address_request` round trip answered from a test hook.
use std::str::FromStr;
use std::time::Duration;

use std::sync::Arc;

use lampo_common::bitcoin::constants::ChainHash;
use lampo_common::bitcoin::Network;
use lampo_common::error;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::hex;
use lampo_common::json;
use lampo_common::ldk::blinded_path::IntroductionNode;
use lampo_common::ldk::offers::offer::Offer;
use lampo_common::ldk::types::payment::PaymentHash;
use lampo_common::model::{request, response};
use lampo_common::types::NodeId;
use lampo_phoenix::conf::PhoenixLspPeer;
use lampo_phoenix::events::PhoenixLspEvent;
use lampo_phoenix::handler::has_feature_bit;
use lampo_phoenix::liquidity_ads::{FundingRate, PaymentType, WillFundRates};
use lampo_phoenix::wire::{
    DnsAddressResponse, FeerateRange, PhoenixLspMessage, RecommendedFeerates, WillAddHtlc,
    ONION_PACKET_LEN,
};
use lampo_testing::prelude::*;
use lampo_testing::{async_wait, LampoTesting};

use crate::init;

const ON_THE_FLY_FUNDING: usize = 561;
const FUNDING_FEE_CREDIT: usize = 563;
const ZERO_RESERVE_CHANNELS: usize = 129;

fn advertises_phoenix_bits(features: &lampo_common::ldk::types::features::InitFeatures) -> bool {
    has_feature_bit(features, ON_THE_FLY_FUNDING)
        && has_feature_bit(features, FUNDING_FEE_CREDIT)
        && !has_feature_bit(features, ZERO_RESERVE_CHANNELS)
}

fn mentions_phoenix_bits(features: &lampo_common::ldk::types::features::InitFeatures) -> bool {
    has_feature_bit(features, ON_THE_FLY_FUNDING)
        || has_feature_bit(features, FUNDING_FEE_CREDIT)
        || has_feature_bit(features, ZERO_RESERVE_CHANNELS)
}

#[tokio_test_shutdown_timeout::test(5)]
pub async fn phoenix_lsp_client_talks_to_its_lsp() -> error::Result<()> {
    init();
    let lsp = LampoTesting::tmp().await?;
    let lsp_id = NodeId::from_str(&lsp.info.node_id)?;
    let lsp_peer = format!("{}@127.0.0.1:{}", lsp.info.node_id, lsp.port);
    let client = LampoTesting::new_with(lsp.btc.clone(), move |conf| {
        conf.set_extension_value("phoenix-lsp", &lsp_peer).unwrap();
    })
    .await?;
    let client_id = NodeId::from_str(&client.info.node_id)?;

    // The client dials its LSP at startup without anyone calling `connect`.
    async_wait!(
        async {
            if client.lampod().peer_manager().is_connected_with(lsp_id) {
                Ok(())
            } else {
                Err(())
            }
        },
        1
    );

    // The LSP sees the Phoenix bits in the client's init features...
    let client_seen_by_lsp = lsp
        .lampod()
        .peer_manager()
        .manager()
        .peer_by_node_id(&client_id)
        .expect("the LSP sees the client");
    assert!(
        advertises_phoenix_bits(&client_seen_by_lsp.init_features),
        "client bits as seen by the LSP: {:?}",
        client_seen_by_lsp.init_features
    );
    // ...while the LSP, which has no LSP of its own, sends none back.
    let lsp_seen_by_client = client
        .lampod()
        .peer_manager()
        .manager()
        .peer_by_node_id(&lsp_id)
        .expect("the client sees the LSP");
    assert!(!mentions_phoenix_bits(&lsp_seen_by_client.init_features));

    // A third node gets none of the bits from the client either.
    let other = LampoTesting::new(lsp.btc.clone()).await?;
    let other_id = NodeId::from_str(&other.info.node_id)?;
    let _: response::Connect = client
        .lampod()
        .call(
            "connect",
            request::Connect {
                node_id: other.info.node_id.clone(),
                addr: "127.0.0.1".to_owned(),
                port: other.port,
            },
        )
        .await?;
    let client_seen_by_other = other
        .lampod()
        .peer_manager()
        .manager()
        .peer_by_node_id(&client_id)
        .expect("the third node sees the client");
    assert!(!mentions_phoenix_bits(&client_seen_by_other.init_features));
    assert!(client.lampod().peer_manager().is_connected_with(other_id));

    // phoenixlsp-info: configured and connected on the client, idle on the LSP.
    let info: response::PhoenixLspInfo = client
        .lampod()
        .call("phoenixlsp-info", json::json!({}))
        .await?;
    assert!(info.configured);
    assert!(info.connected);
    assert_eq!(info.node_id.as_deref(), Some(lsp.info.node_id.as_str()));
    assert_eq!(info.address, Some(format!("127.0.0.1:{}", lsp.port)));
    assert!(info.feerates.is_none());
    assert_eq!(info.fee_credit_msat, 0);
    assert!(info.pending_htlcs.is_empty());
    assert!(info.purchases.is_empty());
    let lsp_features = info
        .lsp_features
        .expect("the LSP's init features are recorded");
    assert!(!lsp_features.on_the_fly_funding);
    let idle: response::PhoenixLspInfo = lsp
        .lampod()
        .call("phoenixlsp-info", json::json!({}))
        .await?;
    assert!(!idle.configured);
    assert!(!idle.connected);

    // The LSP sends recommended_feerates through the test hook; the client
    // reports them.
    let regtest = ChainHash::using_genesis_block_const(Network::Regtest);
    let phoenix_on_lsp = lsp.phoenix();
    phoenix_on_lsp.send_raw(
        client_id,
        PhoenixLspMessage::RecommendedFeerates(RecommendedFeerates {
            chain_hash: regtest,
            funding_feerate: 2_500,
            commitment_feerate: 1_000,
            funding_feerate_range: Some(FeerateRange {
                min: 1_000,
                max: 5_000,
            }),
            commitment_feerate_range: None,
        }),
    );
    lsp.daemon().peer_manager().process_events();
    async_wait!(
        async {
            let info: response::PhoenixLspInfo = client
                .lampod()
                .call("phoenixlsp-info", json::json!({}))
                .await
                .unwrap();
            match info.feerates {
                Some(feerates) if feerates.funding_feerate == 2_500 => {
                    assert_eq!(feerates.commitment_feerate, 1_000);
                    assert_eq!(
                        feerates.funding_feerate_range,
                        Some(response::PhoenixLspFeerateRange {
                            min: 1_000,
                            max: 5_000
                        })
                    );
                    assert!(feerates.commitment_feerate_range.is_none());
                    Ok(())
                }
                _ => Err(()),
            }
        },
        1
    );

    // dns_address_request round trip. The LSP node only accepts Phoenix
    // messages from its own LSP, so point it at the client for the test.
    phoenix_on_lsp.set_lsp_for_test(PhoenixLspPeer::from_str(&format!(
        "{}@127.0.0.1:{}",
        client.info.node_id, client.port
    ))?);
    let mut lsp_events = lsp.lampod().events();
    let client_handler = client.lampod();
    let answer = tokio::spawn(async move {
        client_handler
            .call::<_, response::PhoenixLspDnsAddress>(
                "phoenixlsp-dnsaddress",
                request::PhoenixLspDnsAddress {
                    language: Some("it".to_owned()),
                },
            )
            .await
    });
    let (offer, language) = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), lsp_events.recv())
            .await?
            .ok_or_else(|| error::anyhow!("event bus closed"))?;
        if let Some(PhoenixLspEvent::DnsAddressRequest {
            counterparty_node_id,
            offer,
            language,
        }) = PhoenixLspEvent::from_event(&event)
        {
            assert_eq!(counterparty_node_id, client_id);
            break (offer, language);
        }
    };
    assert_eq!(language, "it");
    // The request carries the client's offer, with the LSP as introduction
    // node of its blinded path even though they share no channel.
    let offer = Offer::try_from(hex::decode(offer)?)
        .map_err(|err| error::anyhow!("offer in dns_address_request: {err:?}"))?;
    assert!(
        offer
            .paths()
            .iter()
            .any(|path| *path.introduction_node() == IntroductionNode::NodeId(lsp_id)),
        "offer paths: {:?}",
        offer.paths()
    );
    phoenix_on_lsp.send_raw(
        client_id,
        PhoenixLspMessage::DnsAddressResponse(DnsAddressResponse {
            chain_hash: regtest,
            address: "alice@lampo.test".to_owned(),
        }),
    );
    lsp.daemon().peer_manager().process_events();
    let answer = answer.await??;
    assert_eq!(answer.address, "alice@lampo.test");

    // An admin-recorded purchase shows up in phoenixlsp-info.
    let purchase: response::PhoenixLspPurchase = client
        .lampod()
        .call(
            "phoenixlsp-recordpurchase",
            request::PhoenixLspRecordPurchase {
                funding_txid: "AB".repeat(32),
                amount_sat: 100_000,
                mining_fee_sat: 1_000,
                service_fee_sat: 2_500,
                payment_type: 128,
                payment_hashes: vec!["11".repeat(32)],
                fee_credit_used_msat: 0,
                created_at: None,
            },
        )
        .await?;
    assert_eq!(purchase.funding_txid, "ab".repeat(32));
    let info: response::PhoenixLspInfo = client
        .lampod()
        .call("phoenixlsp-info", json::json!({}))
        .await?;
    assert_eq!(info.purchases, vec![purchase]);
    Ok(())
}

/// An accepted `will_add_htlc` on a ready channel splices and the LSP sees
/// `request_funding` on `splice_init`.
#[tokio_test_shutdown_timeout::test(8)]
pub async fn accepted_will_add_htlc_requests_liquidity() -> error::Result<()> {
    init();
    let lsp = Arc::new(LampoTesting::tmp().await?);
    let lsp_peer = format!("{}@127.0.0.1:{}", lsp.info.node_id, lsp.port);
    let client = LampoTesting::new_with(lsp.btc.clone(), move |conf| {
        conf.set_extension_value("phoenix-lsp", &lsp_peer).unwrap();
        conf.set_extension_value("phoenix-auto-liquidity", "50000")
            .unwrap();
        conf.set_extension_value("phoenix-max-mining-fee", "10000")
            .unwrap();
    })
    .await?;
    let client_id = NodeId::from_str(&client.info.node_id)?;

    client.fund_wallet(6).await?;
    client.fund_channel_with(lsp.clone(), 200_000).await?;

    let regtest = ChainHash::using_genesis_block_const(Network::Regtest);
    lsp.phoenix().send_raw(
        client_id,
        PhoenixLspMessage::RecommendedFeerates(RecommendedFeerates {
            chain_hash: regtest,
            funding_feerate: 1_000,
            commitment_feerate: 500,
            funding_feerate_range: None,
            commitment_feerate_range: None,
        }),
    );
    lsp.daemon().peer_manager().process_events();
    client
        .phoenix()
        .set_will_fund_rates_for_test(WillFundRates {
            funding_rates: vec![FundingRate {
                min_amount_sat: 10_000,
                max_amount_sat: 1_000_000,
                funding_weight: 112,
                fee_proportional_bps: 0,
                fee_base_sat: 0,
                channel_creation_fee_sat: 0,
            }],
            payment_types: vec![PaymentType::FromFutureHtlc],
        });

    let mut events = client.lampod().events();
    let _invoice: response::Invoice = client
        .lampod()
        .call(
            "invoice",
            request::GenerateInvoice {
                description: "liquidity".to_owned(),
                amount_msat: Some(100_000_000),
                expiring_in: None,
            },
        )
        .await?;
    let payment_hash = loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await?
            .ok_or_else(|| error::anyhow!("event bus closed"))?;
        if let Event::Lightning(LightningEvent::InvoiceIssued { payment_hash, .. }) = event {
            break payment_hash;
        }
    };
    let hash_bytes: [u8; 32] = hex::decode(payment_hash)?
        .try_into()
        .map_err(|_| error::anyhow!("payment hash is not 32 bytes"))?;

    lsp.phoenix().send_raw(
        client_id,
        PhoenixLspMessage::WillAddHtlc(WillAddHtlc {
            chain_hash: regtest,
            id: [7; 32],
            amount_msat: 100_000_000,
            payment_hash: PaymentHash(hash_bytes),
            cltv_expiry: 200,
            onion_routing_packet: Box::new([0; ONION_PACKET_LEN]),
            path_key: None,
        }),
    );

    async_wait!(
        async {
            client.lampod().peer_manager().process_events();
            lsp.lampod().peer_manager().process_events();
            let channel_id = lsp
                .daemon()
                .channel_manager()
                .manager()
                .list_channels()
                .into_iter()
                .find(|channel| {
                    channel.counterparty.node_id == client_id && channel.is_channel_ready
                })
                .map(|channel| channel.channel_id);
            let Some(channel_id) = channel_id else {
                return Err(());
            };
            match lsp
                .daemon()
                .channel_manager()
                .manager()
                .inbound_liquidity_request(&client_id, &channel_id)
            {
                Some(request) if !request.is_empty() => Ok(()),
                _ => Err(()),
            }
        },
        1
    );
    Ok(())
}
