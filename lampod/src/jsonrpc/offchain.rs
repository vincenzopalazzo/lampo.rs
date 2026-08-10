//! Offchain RPC methods
use std::str::FromStr;

use lampo_common::bitcoin::hex::FromHex;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::hex;
use lampo_common::jsonrpc::{Error, RpcError};
use lampo_common::ldk;
use lampo_common::ldk::offers::offer;
use lampo_common::model::request::GenerateOffer;
use lampo_common::model::request::KeySend;
use lampo_common::model::request::Pay;
use lampo_common::model::request::{self, GenerateInvoice};
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
    let result = next_payment_event(ctx, &mut events, &payment_id).await?;
    Ok(json::to_value(result)?)
}

/// How long `pay` waits for a payment to reach a terminal state before
/// giving the caller an answer. A payment that takes longer than this
/// is not failed, it is still in flight: the call gives up waiting, it
/// does not give up on the payment.
const PAYMENT_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Wait for the terminal payment event of `payment_id`, nudging the LDK
/// event queue while waiting (see
/// `LampoDaemon::process_pending_ldk_events`): a manually handled BOLT12
/// invoice does not wake the background processor on its own.
async fn next_payment_event(
    ctx: &LampoDaemon,
    events: &mut lampo_common::chan::UnboundedReceiver<Event>,
    payment_id: &str,
) -> Result<PayResult, Error> {
    // The receipt lands on `PaymentReceipt` and the hop path on the terminal
    // `PaymentEvent`, so hold the receipt until the payment finishes.
    let mut receipt: Option<(String, Option<String>)> = None;
    let deadline = tokio::time::Instant::now() + PAYMENT_WAIT_TIMEOUT;
    loop {
        if tokio::time::Instant::now() >= deadline {
            // Never report this as a failure: the htlcs may still be
            // in flight and the payment can still settle.
            return Err(crate::rpc_error!(
                "the payment did not reach a terminal state within {} seconds, it may still be in flight",
                PAYMENT_WAIT_TIMEOUT.as_secs()
            ));
        }
        ctx.process_pending_ldk_events().await;
        let event = match tokio::time::timeout(std::time::Duration::from_millis(250), events.recv())
            .await
        {
            Ok(Some(event)) => event,
            Ok(None) => {
                return Err(Error::Rpc(RpcError {
                    code: -1,
                    message: format!("No event received, communication channel dropped"),
                    data: None,
                }))
            }
            Err(_) => continue,
        };

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
                return Ok(PayResult {
                    state,
                    path,
                    payment_hash,
                    payment_preimage,
                    payer_proof,
                });
            }
            _ => {}
        }
    }
}

fn parse_payment_id(payment_id: &str) -> Result<ldk::ln::channelmanager::PaymentId, Error> {
    Vec::<u8>::from_hex(payment_id)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .map(ldk::ln::channelmanager::PaymentId)
        .ok_or(crate::rpc_error!(
            "`payment_id` must be a 32 byte hex string"
        ))
}

pub async fn json_fetchinvoice(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!("call for `fetchinvoice` with request `{:?}`", request);
    let request: request::FetchInvoice = json::from_value(request.clone())?;
    // subscribe before firing the request so the event cannot be missed
    let mut events = ctx.handler().events();
    let payment_id = ctx
        .offchain_manager()
        .fetch_invoice_from_offer(&request.offer_str, request.amount_msat, request.payer_note)
        .map_err(|err| crate::rpc_error!("{err}"))?;
    let payment_id_str = payment_id.to_string();

    let wait = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            // nudge the LDK event queue: `InvoiceReceived` does not
            // wake the background processor on its own
            ctx.process_pending_ldk_events().await;
            let event =
                match tokio::time::timeout(std::time::Duration::from_millis(250), events.recv())
                    .await
                {
                    Ok(Some(event)) => event,
                    Ok(None) => {
                        return Err(crate::rpc_error!(
                            "no event received, communication channel dropped"
                        ))
                    }
                    Err(_) => continue,
                };
            if let Event::Lightning(LightningEvent::Bolt12InvoiceReceived {
                payment_id,
                payment_hash,
                amount_msat,
            }) = event
            {
                if payment_id == payment_id_str {
                    return Ok(response::FetchInvoiceResult {
                        payment_id,
                        payment_hash,
                        amount_msat,
                    });
                }
            }
        }
    })
    .await;
    match wait {
        Ok(result) => Ok(json::to_value(&result?)?),
        Err(_) => {
            // give the pending payment back to LDK to reap
            if let Err(err) = ctx.offchain_manager().cancel_fetched_invoice(payment_id) {
                log::warn!(
                    target: "lampo::offchain",
                    "could not cancel the pending fetch `{payment_id}`: {err}"
                );
            }
            Err(crate::rpc_error!(
                "timeout while waiting for the invoice, the offer issuer may be offline"
            ))
        }
    }
}

pub async fn json_payfetched(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!("call for `payfetched` with request `{:?}`", request);
    let request: request::PayFetched = json::from_value(request.clone())?;
    let payment_id = parse_payment_id(&request.payment_id)?;
    let mut events = ctx.handler().events();
    ctx.offchain_manager()
        .pay_fetched_invoice(payment_id)
        .map_err(|err| crate::rpc_error!("{err}"))?;
    // Same rule as `pay`: filter on our own payment id, the bus
    // broadcasts every payment's result to every subscriber.
    let payment_id = hex::encode(payment_id.0);
    let result = next_payment_event(ctx, &mut events, &payment_id).await?;
    Ok(json::to_value(result)?)
}

pub async fn json_cancelfetched(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!("call for `cancelfetched` with request `{:?}`", request);
    let request: request::CancelFetched = json::from_value(request.clone())?;
    let payment_id = parse_payment_id(&request.payment_id)?;
    ctx.offchain_manager()
        .cancel_fetched_invoice(payment_id)
        .map_err(|err| crate::rpc_error!("{err}"))?;
    Ok(json::to_value(&response::CancelFetchedResult {
        payment_id: request.payment_id,
    })?)
}

pub async fn json_holdinvoice(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!("call for `holdinvoice` with request `{:?}`", request);
    let request: request::HoldInvoice = json::from_value(request.clone())?;
    let payment_hash = ldk::types::payment::PaymentHash(
        Vec::<u8>::from_hex(&request.payment_hash)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(crate::rpc_error!(
                "`payment_hash` must be a 32 byte hex string"
            ))?,
    );
    // Register the hold before the invoice leaves the node, so the
    // payment can never arrive without a hold record in place.
    ctx.hold_manager()
        .register(&request.payment_hash, request.amount_msat)
        .map_err(|err| crate::rpc_error!("{err}"))?;
    let invoice = ctx.offchain_manager().generate_invoice_for_hash(
        payment_hash,
        request.amount_msat,
        &request.description,
        request.expiring_in.unwrap_or(10000),
        request.min_final_cltv_expiry_delta,
    );
    let invoice = match invoice {
        Ok(invoice) => invoice,
        Err(err) => {
            // roll the registration back, there is no invoice to pay
            if let Err(err) = ctx.hold_manager().unregister(&request.payment_hash) {
                log::warn!(
                    target: "lampo::hold",
                    "could not roll back the hold for `{}`: {err}", request.payment_hash
                );
            }
            return Err(crate::rpc_error!("{err}"));
        }
    };
    let result = response::HoldInvoiceResult {
        bolt11: invoice.to_string(),
        payment_hash: request.payment_hash,
    };
    Ok(json::to_value(&result)?)
}

pub async fn json_holdclaim(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!("call for `holdclaim`");
    let request: request::HoldClaim = json::from_value(request.clone())?;
    let hold = ctx
        .hold_manager()
        .claim(&request.payment_preimage)
        .map_err(|err| crate::rpc_error!("{err}"))?;
    Ok(json::to_value(&response::HoldClaimResult {
        payment_hash: hold.payment_hash,
    })?)
}

pub async fn json_holdfail(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `holdfail` with request `{:?}`", request);
    let request: request::HoldFail = json::from_value(request.clone())?;
    let hold = ctx
        .hold_manager()
        .fail(&request.payment_hash)
        .map_err(|err| crate::rpc_error!("{err}"))?;
    Ok(json::to_value(&response::HoldFailResult {
        payment_hash: hold.payment_hash,
    })?)
}

pub async fn json_paymentpreimage(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!("call for `paymentpreimage` with request `{:?}`", request);
    let request: request::PaymentPreimage = json::from_value(request.clone())?;
    let payment_hash = ldk::types::payment::PaymentHash(
        Vec::<u8>::from_hex(&request.payment_hash)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or(crate::rpc_error!(
                "`payment_hash` must be a 32 byte hex string"
            ))?,
    );
    // The receipt persisted on `PaymentSent` is the source of truth that
    // an outbound payment settled. Reading it back lets a caller that
    // crashed mid-payment learn the preimage instead of losing it.
    let preimage = crate::ln::payer_proof::load(&ctx.persister(), &payment_hash)
        .map_err(|err| crate::rpc_error!("{err}"))?
        .map(|record| hex::encode(record.preimage.0));
    Ok(json::to_value(&response::PaymentPreimageResult {
        payment_preimage: preimage,
    })?)
}

pub async fn json_listholds(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!("call for `listholds`");
    let _: request::ListHolds = json::from_value(request.clone())?;
    Ok(json::to_value(&response::ListHoldsResult {
        holds: ctx.hold_manager().list(),
    })?)
}

pub async fn json_keysend(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::debug!("call for `keysend` with request `{:?}`", request);
    let request: KeySend = json::from_value(request.clone())?;
    ctx.offchain_manager()
        .keysend(request.destination, request.amount_msat)?;
    // FIXME: return a better response
    Ok(json::json!({}))
}
