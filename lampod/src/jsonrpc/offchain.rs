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
use lampo_common::model::request::Pay;
use lampo_common::model::request::SetAsyncInvoicePaths;
use lampo_common::model::response::PayResult;
use lampo_common::model::response::{self, Decode};
use lampo_common::model::response::{Bolt11InvoiceInfo, Bolt12InvoiceInfo, Invoice};
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
/// Server role only. `recipient_id` is operator-chosen hex; the same bytes
/// key the static-invoice store for this recipient.
pub async fn json_asyncinvoicepaths(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!("call for `asyncinvoicepaths` with request `{:?}`", request);
    let request: GenerateAsyncInvoicePaths = json::from_value(request.clone())?;
    let recipient_id = hex::decode(&request.recipient_id)
        .map_err(|err| crate::rpc_error!("recipient_id is not hex: {err}"))?;
    if recipient_id.is_empty() {
        return Err(crate::rpc_error!("recipient_id must not be empty"));
    }
    let paths = ctx
        .blinded_paths_for_async_recipient(recipient_id)
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
    log::info!(
        "call for `setasyncinvoicepaths` with request `{:?}`",
        request
    );
    let request: SetAsyncInvoicePaths = json::from_value(request.clone())?;
    ctx.set_async_receive_paths_hex(&request.paths)
        .map_err(|err| crate::rpc_error!("{err}"))?;
    Ok(json::to_value(&response::AsyncInvoicePaths {
        paths: request.paths,
    })?)
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
            expiry_time: Some(invoice.expiry_time().as_millis() as u64),
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

    let payment_id = if let Ok(_) = offer::Offer::from_str(&request.invoice_str) {
        log::debug!("Paying offer with bolt12 invoice: {}", request.invoice_str);
        let payer_note = request.bolt12.and_then(|x| x.payer_note);
        ctx.offchain_manager()
            .pay_offer(&request.invoice_str, request.amount, payer_note)?
    } else {
        log::debug!(
            "Paying invoice with bolt11 invoice: {}",
            request.invoice_str
        );
        ctx.offchain_manager()
            .pay_invoice(&request.invoice_str, request.amount)?
    };
    // The event bus broadcasts to every subscriber, so a concurrent `pay` would
    // otherwise see this payment's result -- and now its preimage and payer
    // proof too. Only accept events carrying our own payment id.
    let payment_id = hex::encode(payment_id.0);
    wait_for_payment_result(events, &payment_id, request.timeout.duration()).await
}

/// Hold the `PaymentReceipt` (preimage, payer proof) until the terminal
/// `PaymentEvent` for `payment_id` arrives, and build the `PayResult`.
/// The event bus broadcasts to every subscriber, so only events carrying
/// our own payment id are accepted -- a concurrent payment must not leak
/// its result into ours.
async fn wait_for_payment_result(
    mut events: lampo_common::chan::UnboundedReceiver<Event>,
    payment_id: &str,
    timeout: Duration,
) -> Result<json::Value, Error> {
    // Single deadline for the whole RPC wait. The event bus is broadcast, so
    // unrelated events must not reset the timer — only the terminal
    // `PaymentEvent` for `payment_id` completes the call (success or failure).
    // If that event never arrives, stop waiting after `timeout` instead of
    // blocking forever; the payment itself may still be retried in the background.
    let deadline = Instant::now() + timeout;

    // The receipt lands on `PaymentReceipt` and the hop path on the terminal
    // `PaymentEvent`, so hold the receipt until the payment finishes.
    let mut receipt: Option<(String, Option<String>)> = None;

    loop {
        log::warn!(target: "lampod::jsonrpc::offchain", "Waiting for payment event...");
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .map_err(|_| {
                Error::Rpc(RpcError {
                    code: -1,
                    message: format!(
                        "payment `{}` did not complete within {}s (no terminal Payment event; \
                         payment status unknown — it may still be retried in the background)",
                        payment_id,
                        timeout.as_secs()
                    ),
                    data: None,
                })
            })?
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
            }
            Event::Lightning(LightningEvent::PaymentEvent {
                payment_id: Some(id),
                payment_hash,
                path,
                state,
                reason: _,
            }) if id == payment_id => {
                let (payment_preimage, payer_proof) = match receipt {
                    Some((preimage, proof)) => (Some(preimage), proof),
                    None => (None, None),
                };
                return Ok(json::to_value(PayResult {
                    state,
                    path,
                    payment_hash,
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
    wait_for_payment_result(events, &payment_id, request.timeout.duration()).await
}
