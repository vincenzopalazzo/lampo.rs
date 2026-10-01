//! Phoenix LSP RPC methods
use std::time::Duration;

use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::hex;
use lampo_common::json;
use lampo_common::jsonrpc::{Error, RpcError};
use lampo_common::ldk::util::ser::Writeable;
use lampo_common::model::request::{PhoenixLspDnsAddress, PhoenixLspRecordPurchase};
use lampo_common::model::response;

use crate::ln::phoenix_lsp::handler::{
    feature_bits, supports_feature, FUNDING_FEE_CREDIT_BIT, ON_THE_FLY_FUNDING_BIT,
    ZERO_RESERVE_CHANNELS_BIT,
};
use crate::ln::phoenix_lsp::liquidity_ads::PaymentType;
use crate::ln::phoenix_lsp::purchases::{unix_now, Purchase};
use crate::LampoDaemon;

/// How long `phoenixlsp-dnsaddress` waits for the LSP's answer.
const DNS_ADDRESS_TIMEOUT: Duration = Duration::from_secs(30);

pub async fn json_phoenixlsp_info(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!("call for `phoenixlsp-info` with request `{:?}`", request);
    let phoenix_lsp = ctx.phoenix_lsp();
    let snapshot = phoenix_lsp.snapshot();
    let lsp_features =
        snapshot
            .lsp_init_features
            .as_ref()
            .map(|features| response::PhoenixLspFeatures {
                on_the_fly_funding: supports_feature(features, ON_THE_FLY_FUNDING_BIT - 1),
                funding_fee_credit: supports_feature(features, FUNDING_FEE_CREDIT_BIT - 1),
                zero_reserve_channels: supports_feature(features, ZERO_RESERVE_CHANNELS_BIT - 1),
                bits: feature_bits(features),
            });
    let feerates = snapshot
        .feerates
        .as_ref()
        .map(|feerates| response::PhoenixLspFeerates {
            funding_feerate: feerates.funding_feerate,
            commitment_feerate: feerates.commitment_feerate,
            funding_feerate_range: feerates.funding_feerate_range.map(|range| {
                response::PhoenixLspFeerateRange {
                    min: range.min,
                    max: range.max,
                }
            }),
            commitment_feerate_range: feerates.commitment_feerate_range.map(|range| {
                response::PhoenixLspFeerateRange {
                    min: range.min,
                    max: range.max,
                }
            }),
        });
    let pending_htlcs = snapshot
        .pending
        .iter()
        .map(|pending| response::PhoenixLspPendingHtlc {
            id: hex::encode(pending.msg.id),
            amount_msat: pending.msg.amount_msat,
            payment_hash: hex::encode(pending.msg.payment_hash.0),
            cltv_expiry: pending.msg.cltv_expiry,
            received_at: pending.received_at,
            decision: pending.decision.to_string(),
        })
        .collect();
    let info = response::PhoenixLspInfo {
        configured: snapshot.lsp.is_some(),
        node_id: snapshot.lsp.as_ref().map(|lsp| lsp.node_id.to_string()),
        address: snapshot
            .lsp
            .as_ref()
            .map(|lsp| format!("{}:{}", lsp.host, lsp.port)),
        connected: snapshot.connected,
        lsp_features,
        feerates,
        fee_credit_msat: snapshot.fee_credit_msat,
        pending_htlcs,
        purchases: phoenix_lsp.purchases().list(),
    };
    Ok(json::to_value(&info)?)
}

/// Build this node's offer with the LSP as introduction node, send a
/// `dns_address_request` for it and wait for the address.
pub async fn json_phoenixlsp_dnsaddress(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!(
        "call for `phoenixlsp-dnsaddress` with request `{:?}`",
        request
    );
    let request: PhoenixLspDnsAddress = json::from_value(request.clone())?;
    let language = request
        .language
        .map(|language| language.trim().to_owned())
        .filter(|language| !language.is_empty())
        .unwrap_or_else(|| "en".to_owned());
    let phoenix_lsp = ctx.phoenix_lsp();
    let Some(lsp) = phoenix_lsp.lsp_node_id() else {
        return Err(crate::rpc_error!("no phoenix-lsp configured"));
    };
    if !phoenix_lsp.is_connected() {
        return Err(crate::rpc_error!("phoenix LSP `{lsp}` is not connected"));
    }
    // Subscribe before sending so the answer cannot slip past us.
    let mut events = ctx.handler().events();
    let offer = ctx
        .offchain_manager()
        .offer_via_introduction_node(lsp)
        .map_err(|err| crate::rpc_error!("{err}"))?;
    log::info!(
        target: "phoenix-lsp",
        "requesting a BIP 353 address in `{language}` for offer `{offer}`"
    );
    phoenix_lsp.send_dns_address_request(offer.encode(), &language)?;
    // Queued messages leave with the next peer-manager pass; do not wait for
    // the background processor's timer.
    ctx.peer_manager().process_events();

    let deadline = tokio::time::Instant::now() + DNS_ADDRESS_TIMEOUT;
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .map_err(|_| {
                crate::rpc_error!(
                    "the Phoenix LSP did not answer the dns_address_request within {}s",
                    DNS_ADDRESS_TIMEOUT.as_secs()
                )
            })?
            .ok_or_else(|| crate::rpc_error!("event bus closed while waiting for the LSP"))?;
        if let Event::Lightning(LightningEvent::PhoenixLspDnsAddress { address }) = event {
            return Ok(json::to_value(response::PhoenixLspDnsAddress { address })?);
        }
    }
}

/// Admin entry of a liquidity purchase, every field explicit. It exists
/// so claiming an HTLC that carries an LSP funding fee can be tested end to
/// end before this node can buy liquidity itself.
pub async fn json_phoenixlsp_recordpurchase(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!(
        "call for `phoenixlsp-recordpurchase` with request `{:?}`",
        request
    );
    let request: PhoenixLspRecordPurchase = json::from_value(request.clone())?;
    let funding_txid = parse_hex32(&request.funding_txid, "funding_txid")?;
    if let PaymentType::Unknown(bit) = PaymentType::from_bit(request.payment_type) {
        return Err(crate::rpc_error!(
            "unknown payment_type {bit}: expected 0, 128, 129 or 130"
        ));
    }
    if request.payment_type != 0 && request.payment_hashes.is_empty() {
        return Err(crate::rpc_error!(
            "payment_type {} needs at least one payment hash",
            request.payment_type
        ));
    }
    let payment_hashes = request
        .payment_hashes
        .iter()
        .map(|hash| parse_hex32(hash, "payment_hashes"))
        .collect::<Result<Vec<_>, _>>()?;
    let purchase = Purchase {
        funding_txid,
        amount_sat: request.amount_sat,
        mining_fee_sat: request.mining_fee_sat,
        service_fee_sat: request.service_fee_sat,
        payment_type: request.payment_type,
        payment_hashes,
        fee_credit_used_msat: request.fee_credit_used_msat,
        created_at: request.created_at.unwrap_or_else(unix_now),
    };
    ctx.phoenix_lsp().purchases().upsert(purchase.clone())?;
    log::info!(
        target: "phoenix-lsp",
        "recorded purchase `{}`: {} sat, fees {} + {} sat, payment type {}",
        purchase.funding_txid,
        purchase.amount_sat,
        purchase.mining_fee_sat,
        purchase.service_fee_sat,
        purchase.payment_type
    );
    Ok(json::to_value(&purchase)?)
}

/// Validate a hex-encoded 32-byte value and return it lowercased.
fn parse_hex32(raw: &str, field: &str) -> Result<String, Error> {
    let bytes = hex::decode(raw.trim())
        .ok()
        .filter(|bytes| bytes.len() == 32)
        .ok_or_else(|| crate::rpc_error!("invalid {field} `{raw}`: expected 32 hex bytes"))?;
    Ok(hex::encode(bytes))
}
