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
use lampo_common::model::request::GenerateInvoice;
use lampo_common::model::request::GenerateOffer;
use lampo_common::model::request::KeySend;
use lampo_common::model::request::Pay;
use lampo_common::model::response::PayResult;
use lampo_common::model::response::{self, Decode};
use lampo_common::model::response::{Bolt11InvoiceInfo, Bolt12InvoiceInfo, Invoice};
use lampo_common::{json, model::request::DecodeInvoice};

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
    // Upper bound for how long a single `pay` call waits for the terminal
    // PaymentEvent. Without it the handler below waits forever whenever the
    // event is never generated — observed live after an unclean node
    // restart, where LDK logs `Failed to update channel monitor: no such
    // monitor registered`, the payment stalls and no failure event fires.
    // The payment itself may still be retried in the background; the RPC
    // simply stops blocking.
    const PAY_EVENT_TIMEOUT: Duration = Duration::from_secs(120);

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

    // The receipt lands on `PaymentReceipt` and the hop path on the terminal
    // `PaymentEvent`, so hold the receipt until the payment finishes.
    let mut receipt: Option<(String, Option<String>)> = None;

    // FIXME: this will loop when the Payment event is not generated
    // If the terminal PaymentEvent is never generated (e.g. the payment
    // stalls after an unclean restart), stop waiting after
    // PAY_EVENT_TIMEOUT instead of blocking the caller forever.
    loop {
        log::warn!("Waiting for payment event...");
        let event = tokio::time::timeout(PAY_EVENT_TIMEOUT, events.recv())
            .await
            .map_err(|_| {
                Error::Rpc(RpcError {
                    code: -1,
                    message: format!(
                        "payment `{}` did not complete within {}s (no Payment event; \
                     the payment may still be retried in the background)",
                        payment_id,
                        PAY_EVENT_TIMEOUT.as_secs()
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
    // Same bound as `pay`: without it the handler would block forever
    // whenever the terminal PaymentEvent is never generated.
    const KEYSEND_EVENT_TIMEOUT: Duration = Duration::from_secs(120);

    let request: KeySend = json::from_value(request.clone())?;
    let mut events = ctx.handler().events();
    let payment_hash = ctx
        .offchain_manager()
        .keysend(request.destination, request.amount_msat)?;

    // `OffchainManager::keysend` registers the payment with
    // `PaymentId(payment_hash.0)`, so the terminal PaymentEvent carries the
    // hash as its payment id. Returning it also gives callers a handle to
    // correlate the payment (see issue #567).
    let payment_id = hex::encode(payment_hash.0);

    // Mirror `json_pay`: hold the PaymentReceipt (preimage) until the
    // terminal PaymentEvent arrives, then answer with the full result
    // instead of an empty object — keysend outcomes were unauditable.
    let mut receipt: Option<(String, Option<String>)> = None;

    loop {
        let event = tokio::time::timeout(KEYSEND_EVENT_TIMEOUT, events.recv())
            .await
            .map_err(|_| {
                Error::Rpc(RpcError {
                    code: -1,
                    message: format!(
                        "keysend `{}` did not complete within {}s (no Payment event; \
                         the payment may still be retried in the background)",
                        payment_id,
                        KEYSEND_EVENT_TIMEOUT.as_secs()
                    ),
                    data: None,
                })
            })?
            .ok_or(Error::Rpc(RpcError {
                code: -1,
                message: "No event received, communication channel dropped".to_string(),
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

/// List the in-memory payment history (issue #567): every terminal sent
/// payment and every claimed inbound payment since node start.
pub async fn json_listpayments(
    ctx: &LampoDaemon,
    _request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!("call for `listpayments`");
    Ok(json::to_value(ctx.payments().list())?)
}
