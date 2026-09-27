//! Offchain RPC methods
use std::str::FromStr;
use std::time::Duration;

use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::hex;
use lampo_common::jsonrpc::{Error, RpcError};
use lampo_common::ldk;
use lampo_common::ldk::offers::offer;
use lampo_common::ldk::util::ser::Writeable;
use lampo_common::model::request::GenerateAsyncInvoicePaths;
use lampo_common::model::request::GenerateInvoice;
use lampo_common::model::request::GenerateOffer;
use lampo_common::model::request::KeySend;
use lampo_common::model::request::SetAsyncInvoicePaths;
use lampo_common::model::request::{self, Pay};
use lampo_common::model::response::PayResult;
use lampo_common::model::response::{self, Decode};
use lampo_common::model::response::{Bolt11InvoiceInfo, Bolt12InvoiceInfo, Invoice};
use lampo_common::persist::{PaymentDirection, PaymentStatus};
use lampo_common::types::NodeId;
use lampo_common::{json, model::request::DecodeInvoice};
use tokio::time::Instant;

use crate::LampoDaemon;

pub async fn json_invoice(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `invoice` with request `{:?}`", request);
    let request: GenerateInvoice = json::from_value(request.clone())?;
    let invoice = ctx.offchain_manager().generate_invoice(
        request.amount_msat,
        &request.description,
        request.expiring_in.unwrap_or(10000),
    )?;
    let invoice = Invoice {
        bolt11: invoice.to_string(),
    };
    Ok(json::to_value(&invoice)?)
}

pub async fn json_offer(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `offer` with request `{:?}`", request);
    let request: GenerateOffer = json::from_value(request.clone())?;

    // An async recipient's offer is built interactively with its static
    // invoice server; description/amount cannot be applied to it.
    if ctx.offchain_manager().async_receive_enabled() {
        if request.description.is_some() || request.amount_msat.is_some() {
            return Err(crate::rpc_error!(
                "description/amount_msat cannot be set on an async receive offer; the offer is built with the static invoice server"
            ));
        }
        let offer: response::Offer = ctx
            .offchain_manager()
            .async_offer()
            .map_err(|err| crate::rpc_error!("{err:?}"))?
            .into();
        return Ok(json::to_value(&offer)?);
    }

    let manager = ctx.channel_manager().manager();
    let mut offer_builder = manager
        .create_offer_builder()
        .map_err(|err| crate::rpc_error!("{:?}", err))?;

    if let Some(description) = request.description {
        offer_builder = offer_builder.description(description);
    }

    if let Some(amount_msat) = request.amount_msat {
        offer_builder = offer_builder.amount_msats(amount_msat);
    }

    let offer: response::Offer = offer_builder
        .build()
        // FIXME: implement display error on top of the bolt12 error
        .map_err(|err| crate::rpc_error!("{:?}", err))?
        .into();
    log::debug!("Generated offer: {:?}", offer);
    Ok(json::to_value(&offer)?)
}

/// Mint hex-encoded blinded paths that an often-offline recipient installs
/// as `async-invoice-server-paths` (or via `setasyncinvoicepaths`).
///
/// Server role only. `node_id` must be a channel counterparty; those
/// pubkey bytes key the static-invoice store for this recipient.
pub async fn json_asyncinvoicepaths(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!(target: "lampod::jsonrpc::offchain", "call for `asyncinvoicepaths`");
    let request: GenerateAsyncInvoicePaths = json::from_value(request.clone())?;
    require_path_rpc_token(ctx, request.token.as_deref())?;
    let node_id = NodeId::from_str(&request.node_id)
        .map_err(|err| crate::rpc_error!("node_id is not a public key: {err}"))?;
    if ctx
        .channel_manager()
        .manager()
        .list_channels_with_counterparty(&node_id)
        .is_empty()
    {
        return Err(crate::rpc_error!("node_id must be a channel counterparty"));
    }
    let paths = ctx
        .blinded_paths_for_async_recipient(node_id.serialize().to_vec())
        .map_err(|err| crate::rpc_error!("{err}"))?;
    let paths = hex::encode(paths.encode());
    Ok(json::to_value(&response::AsyncInvoicePaths { paths })?)
}

/// Install hex-encoded blinded paths on an often-offline recipient.
/// Runtime equivalent of the `async-invoice-server-paths` config key.
pub async fn json_setasyncinvoicepaths(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!(target: "lampod::jsonrpc::offchain", "call for `setasyncinvoicepaths`");
    let request: SetAsyncInvoicePaths = json::from_value(request.clone())?;
    require_path_rpc_token(ctx, request.token.as_deref())?;
    if ctx.conf().async_payments_role.as_deref() == Some("server") && !request.force {
        return Err(crate::rpc_error!(
            "setasyncinvoicepaths is for often-offline recipients, not async-payments-role=server"
        ));
    }
    if ctx.offchain_manager().async_receive_enabled() && !request.force {
        return Err(crate::rpc_error!(
            "async receive paths already set; pass force=true to overwrite"
        ));
    }
    ctx.offchain_manager()
        .set_async_receive_paths_hex(&request.paths)
        .map_err(|err| crate::rpc_error!("{err}"))?;
    Ok(json::to_value(&response::AsyncInvoicePaths {
        paths: request.paths,
    })?)
}

/// When `api-token` is set, the two path RPCs require a matching token.
/// Missing and wrong tokens share one error so the token is not enumerable.
fn require_path_rpc_token(ctx: &LampoDaemon, token: Option<&str>) -> Result<(), Error> {
    let conf = ctx.conf();
    let Some(expected) = conf.api_token.as_deref() else {
        return Ok(());
    };
    if token == Some(expected) {
        return Ok(());
    }
    Err(crate::rpc_error!("api-token required"))
}

pub async fn json_decode(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `invoice` with request `{:?}`", request);
    let request: DecodeInvoice = json::from_value(request.clone())?;
    if let Ok(invoice) = ctx
        .offchain_manager()
        .decode::<ldk::invoice::Bolt11Invoice>(&request.invoice_str)
    {
        let bolt11_invoice = Bolt11InvoiceInfo {
            issuer_id: invoice.payee_pub_key().map(|id| id.to_string()),
            payment_hash: hex::encode(invoice.payment_hash().0),
            timestamp: invoice.duration_since_epoch().as_secs(),
            amount_msat: invoice.amount_milli_satoshis(),
            network: invoice.network().to_string(),
            description: match invoice.description() {
                ldk::invoice::Bolt11InvoiceDescriptionRef::Direct(dec) => Some(dec.to_string()),
                ldk::invoice::Bolt11InvoiceDescriptionRef::Hash(_) => {
                    Some("description hash provided".to_string())
                }
            },
            routes: Vec::new(),
            hints: Vec::new(),
            expiry_time: Some(invoice.expiry_time().as_secs()),
        };

        return Ok(json::to_value(&Decode::from(bolt11_invoice))?);
    }

    if let Ok(offer) = ctx
        .offchain_manager()
        .decode::<ldk::offers::offer::Offer>(&request.invoice_str)
    {
        let bolt12_invoice: Bolt12InvoiceInfo = offer.into();
        return Ok(json::to_value(&Decode::from(bolt12_invoice))?);
    } else {
        Err(crate::rpc_error!("Not able to decode invoice"))
    }
}

pub async fn json_pay(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `pay` with request `{:?}`", request);
    let request: Pay = json::from_value(request.clone())?;
    let mut events = ctx.handler().events();

    let parsed_offer = offer::Offer::from_str(&request.invoice_str);
    let expected_value_msat = match &parsed_offer {
        Ok(offer) => match offer.amount() {
            Some(offer::Amount::Bitcoin { amount_msats }) => Some(amount_msats),
            _ => request.amount,
        },
        Err(_) => ctx
            .offchain_manager()
            .decode_invoice(&request.invoice_str)
            .ok()
            .and_then(|invoice| invoice.amount_milli_satoshis())
            .or(request.amount),
    };

    let payment_id = if parsed_offer.is_ok() {
        log::debug!("Paying offer with bolt12 invoice: {}", request.invoice_str);
        let payer_note = request.bolt12.and_then(|x| x.payer_note);
        ctx.offchain_manager().pay_offer(
            &request.invoice_str,
            request.amount,
            payer_note,
            request.max_fee_msat,
        )?
    } else {
        log::debug!(
            "Paying invoice with bolt11 invoice: {}",
            request.invoice_str
        );
        ctx.offchain_manager().pay_invoice(
            &request.invoice_str,
            request.amount,
            request.max_fee_msat,
            request.timeout_secs,
        )?
    };
    // The event bus broadcasts to every subscriber, so a concurrent `pay` would
    // otherwise see this payment's result -- and now its preimage and payer
    // proof too. Only accept events carrying our own payment id.
    let payment_id = hex::encode(payment_id.0);
    // `pay_offer` returns a PaymentId, not the eventual payment hash. Recording
    // that id as the hash races PaymentSent: a fast success keeps its status
    // but would then have the hash overwritten by this pending write. Leave
    // the hash empty until the terminal event supplies the real one. BOLT 11
    // still records the hash, which is the payment id.
    let recorded_hash = if parsed_offer.is_ok() {
        String::new()
    } else {
        payment_id.clone()
    };
    ctx.handler().record_payment(
        payment_id.clone(),
        recorded_hash,
        PaymentDirection::Outbound,
        expected_value_msat.unwrap_or_default(),
        None,
        PaymentStatus::Pending,
        Some(request.invoice_str),
    );
    // LND-compatible callers use `timeout_secs` as the retry deadline before
    // an HTLC is launched. Once the payment API accepts an initial route, wait
    // for the real terminal event so an in-flight payment is never reported as
    // failed merely because that deadline elapsed.
    let timeout = terminal_wait_timeout(request.timeout_secs, request.timeout.duration());
    wait_for_payment_result(events, &payment_id, expected_value_msat, timeout).await
}

fn terminal_wait_timeout(
    compatible_retry_timeout_secs: Option<u64>,
    default_timeout: Duration,
) -> Option<Duration> {
    compatible_retry_timeout_secs
        .is_none()
        .then_some(default_timeout)
}

/// Hold the `PaymentReceipt` (preimage, payer proof) until the terminal
/// `PaymentEvent` for `payment_id` arrives, and build the `PayResult`.
/// The event bus broadcasts to every subscriber, so only events carrying
/// our own payment id are accepted -- a concurrent payment must not leak
/// its result into ours.
async fn wait_for_payment_result(
    mut events: lampo_common::chan::UnboundedReceiver<Event>,
    payment_id: &str,
    expected_value_msat: Option<u64>,
    timeout: Option<Duration>,
) -> Result<json::Value, Error> {
    // Regular JSON-RPC calls retain a single deadline for the whole wait.
    // LND-compatible calls have no terminal deadline because their timeout only
    // limits the period before launching an HTLC.
    let deadline = timeout.map(|timeout| Instant::now() + timeout);

    // The receipt lands on `PaymentReceipt` and the hop path on the terminal
    // `PaymentEvent`, so hold the receipt until the payment finishes.
    let mut receipt: Option<(String, Option<String>)> = None;
    let mut successful_path = Vec::new();
    let mut value_msat = 0_u64;
    let mut fee_msat = 0_u64;
    let mut successful_payment_hash = None;

    loop {
        log::warn!(target: "lampod::jsonrpc::offchain", "Waiting for payment event...");
        let event = if let Some(deadline) = deadline {
            tokio::time::timeout_at(deadline, events.recv())
                .await
                .map_err(|_| {
                    Error::Rpc(RpcError {
                        code: -1,
                        message: format!(
                            "payment `{}` did not complete within {}s (no terminal Payment event; \
                             payment status unknown — it may still be retried in the background)",
                            payment_id,
                            timeout.map(|value| value.as_secs()).unwrap_or_default()
                        ),
                        data: None,
                    })
                })?
        } else {
            events.recv().await
        }
        .ok_or(Error::Rpc(RpcError {
            code: -1,
            message: format!("No event received, communication channel dropped"),
            data: None,
        }))?;

        match event {
            Event::Lightning(LightningEvent::PaymentReceipt {
                payment_id: id,
                payment_preimage,
                payer_proof,
            }) if id == payment_id => {
                receipt = Some((payment_preimage, payer_proof));
                // Path success can arrive first. Once we have the receipt
                // and a successful path, the payment is done.
                if !successful_path.is_empty() {
                    let (payment_preimage, payer_proof) = receipt
                        .clone()
                        .map(|(preimage, proof)| (Some(preimage), proof))
                        .unwrap_or((None, None));
                    return Ok(json::to_value(PayResult {
                        state: lampo_common::model::response::PaymentState::Success,
                        path: successful_path,
                        payment_hash: successful_payment_hash,
                        value_msat: expected_value_msat.unwrap_or(value_msat),
                        fee_msat,
                        reason: None,
                        payment_preimage,
                        payer_proof,
                    })?);
                }
            }
            Event::Lightning(LightningEvent::PaymentEvent {
                payment_id: Some(id),
                payment_hash,
                path,
                state,
                reason,
            }) if id == payment_id => {
                if state == lampo_common::model::response::PaymentState::Success {
                    let (path_value_msat, path_fee_msat) = path_value_and_fee(&path);
                    value_msat = value_msat.saturating_add(path_value_msat);
                    fee_msat = fee_msat.saturating_add(path_fee_msat);
                    successful_path.extend(path);
                    successful_payment_hash =
                        successful_payment_hash.or_else(|| payment_hash.clone());
                    // Last-hop `fee_msat` is the intro-node / blinded-path
                    // *fee*, not the payment value (async static-invoice
                    // path: hop_fee=1000, invoice=100000). Waiting until
                    // hop fees sum to the invoice never finishes. ldk-node
                    // treats PaymentSent as terminal and does not compare
                    // hop fees. Fill the reported value from the invoice
                    // when hops do not carry it.
                    if value_msat < expected_value_msat.unwrap_or(0) {
                        value_msat = expected_value_msat.unwrap_or(value_msat);
                    }
                    // PaymentPathSuccessful can race PaymentSent. Hold the
                    // Success until the receipt (preimage) lands so `pay`
                    // does not return without it.
                    if receipt.is_none() {
                        continue;
                    }
                }
                let (payment_preimage, payer_proof) = match receipt {
                    Some((preimage, proof)) => (Some(preimage), proof),
                    None => (None, None),
                };
                return Ok(json::to_value(PayResult {
                    state,
                    path: successful_path,
                    payment_hash: successful_payment_hash.or(payment_hash),
                    value_msat,
                    fee_msat,
                    reason,
                    payment_preimage,
                    payer_proof,
                })?);
            }
            _ => {}
        }
    }
}

pub async fn json_keysend(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `keysend` with request `{:?}`", request);
    let request: KeySend = json::from_value(request.clone())?;
    let destination = request.destination()?;
    let mut events = ctx.handler().events();
    let payment_id = ctx
        .offchain_manager()
        .keysend(destination, request.amount_msat)?;
    // Same id semantics as `pay`: the hex payment hash identifies the
    // payment on the event bus.
    let payment_id = hex::encode(payment_id.0);
    ctx.handler().record_payment(
        payment_id.clone(),
        payment_id.clone(),
        PaymentDirection::Outbound,
        request.amount_msat,
        None,
        PaymentStatus::Pending,
        None,
    );
    wait_for_payment_result(
        events,
        &payment_id,
        Some(request.amount_msat),
        Some(request.timeout.duration()),
    )
    .await
}

fn path_value_and_fee(path: &[response::PaymentHop]) -> (u64, u64) {
    let value_msat = path.last().map(|hop| hop.hop_fee_msat).unwrap_or(0);
    let total_msat = path
        .iter()
        .fold(0_u64, |total, hop| total.saturating_add(hop.hop_fee_msat));
    (value_msat, total_msat.saturating_sub(value_msat))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatible_retry_deadline_does_not_end_terminal_wait() {
        assert_eq!(
            terminal_wait_timeout(Some(60), Duration::from_secs(120)),
            None
        );
        assert_eq!(
            terminal_wait_timeout(None, Duration::from_secs(120)),
            Some(Duration::from_secs(120))
        );
    }

    #[test]
    fn aggregates_value_and_fees_per_mpp_path() {
        let hop = |fee| response::PaymentHop {
            node_id: String::new(),
            short_channel_id: 0,
            hop_fee_msat: fee,
            cltv_expiry_delta: 0,
            private_hop: false,
        };

        assert_eq!(path_value_and_fee(&[hop(20), hop(400)]), (400, 20));
        assert_eq!(path_value_and_fee(&[hop(30), hop(600)]), (600, 30));
    }
}

/// `listpayments`: the node's payment history, straight out of the store.
///
/// The filtering happens in the store rather than here.
pub async fn json_listpayments(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::debug!(target: "lampod::jsonrpc::offchain", "call for `listpayments` with request `{:?}`", request);
    let request: request::ListPayments = json::from_value(request.clone())?;
    let filter = request.to_filter().map_err(|err| {
        Error::Rpc(RpcError {
            code: -1,
            message: format!("{err}"),
            data: None,
        })
    })?;
    let payments = ctx.persister().list_payments(&filter).map_err(|err| {
        Error::Rpc(RpcError {
            code: -1,
            message: format!("{err}"),
            data: None,
        })
    })?;
    let response = response::ListPayments {
        payments: payments.into_iter().map(response::Payment::from).collect(),
    };
    Ok(json::to_value(response)?)
}
