//! Offchain RPC methods
use std::str::FromStr;
use std::time::Duration;

use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::hex;
use lampo_common::jsonrpc::{Error, RpcError};
use lampo_common::ldk;
use lampo_common::ldk::offers::offer::{self, Amount};
use lampo_common::model::request::GenerateInvoice;
use lampo_common::model::request::GenerateOffer;
use lampo_common::model::request::KeySend;
use lampo_common::model::request::{self, Pay};
use lampo_common::model::response::PayResult;
use lampo_common::model::response::{self, Decode};
use lampo_common::model::response::{Bolt11InvoiceInfo, Bolt12InvoiceInfo, Invoice};
use lampo_common::persist::{PaymentDirection, PaymentStatus};
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

    let (payment_id, payment_hash, amount_msat) = if let Ok(offer) =
        offer::Offer::from_str(&request.invoice_str)
    {
        log::debug!("Paying offer with bolt12 invoice: {}", request.invoice_str);
        let amount_msat = match offer.amount() {
            Some(Amount::Bitcoin { amount_msats }) => amount_msats,
            Some(_) | None => request.amount.unwrap_or_default(),
        };
        let payer_note = request.bolt12.and_then(|x| x.payer_note);
        (
            ctx.offchain_manager()
                .pay_offer(&request.invoice_str, request.amount, payer_note)?,
            String::new(),
            amount_msat,
        )
    } else {
        log::debug!(
            "Paying invoice with bolt11 invoice: {}",
            request.invoice_str
        );
        let invoice = ctx
            .offchain_manager()
            .decode_invoice(&request.invoice_str)?;
        let amount_msat = invoice
            .amount_milli_satoshis()
            .or(request.amount)
            .unwrap_or_default();
        (
            ctx.offchain_manager()
                .pay_invoice(&request.invoice_str, request.amount)?,
            invoice.payment_hash().to_string(),
            amount_msat,
        )
    };
    // The event bus broadcasts to every subscriber, so a concurrent `pay` would
    // otherwise see this payment's result -- and now its preimage and payer
    // proof too. Only accept events carrying our own payment id.
    let payment_id = hex::encode(payment_id.0);
    ctx.handler().record_payment(
        payment_id.clone(),
        payment_hash,
        PaymentDirection::Outbound,
        amount_msat,
        None,
        PaymentStatus::Pending,
        Some(request.invoice_str),
    );
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
    ctx.handler().record_payment(
        payment_id.clone(),
        payment_id.clone(),
        PaymentDirection::Outbound,
        request.amount_msat,
        None,
        PaymentStatus::Pending,
        None,
    );
    wait_for_payment_result(events, &payment_id, request.timeout.duration()).await
}

/// `listpayments`: the node's payment history, straight out of the store.
///
/// The filtering happens in the store rather than here, so a database backend
/// answers a time window from an index instead of walking every payment.
pub async fn json_listpayments(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!("call for `listpayments` with request `{:?}`", request);
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
