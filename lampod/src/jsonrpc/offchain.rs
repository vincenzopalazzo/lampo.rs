//! Offchain RPC methods
use std::str::FromStr;
use std::time::Duration;

use lampo_common::conf::Network;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::ldk::offers::offer;
use lampo_common::ldk::offers::offer::Amount;
use lampo_common::model::request::{DecodeInvoice, GenerateInvoice, GenerateOffer, KeySend, Pay};
use lampo_common::model::response;
use lampo_common::model::response::{Invoice, InvoiceInfo, PayResult};
use lampo_common::{json, ldk};
use lampo_jsonrpc::errors::{Error, RpcError};

use crate::LampoDaemon;

pub fn json_invoice(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
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

pub fn json_offer(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
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
    Ok(json::to_value(&offer)?)
}

pub fn json_decode_invoice(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `invoice` with request `{:?}`", request);
    let request: DecodeInvoice = json::from_value(request.clone())?;

    let invoice = if let Ok(invoice) = ctx
        .offchain_manager()
        .decode::<ldk::invoice::Bolt11Invoice>(&request.invoice_str)
    {
        InvoiceInfo {
            issuer_id: invoice.payee_pub_key().map(|id| id.to_string()),
            amount_msat: invoice.amount_milli_satoshis(),
            network: invoice.network().to_string(),
            description: match invoice.description() {
                ldk::invoice::Bolt11InvoiceDescription::Direct(dec) => Some(dec.to_string()),
                ldk::invoice::Bolt11InvoiceDescription::Hash(_) => {
                    Some("description hash provided".to_string())
                }
            },
            routes: Vec::new(),
            hints: Vec::new(),
            expiry_time: Some(invoice.expiry_time().as_millis() as u64),
        }
    } else if let Ok(offer) = ctx
        .offchain_manager()
        .decode::<ldk::offers::offer::Offer>(&request.invoice_str)
    {
        // FIXME: simplify this chain;
        let network = offer
            .chains()
            .iter()
            .map(|chain| Network::from_chain_hash(chain.clone()).unwrap().to_string())
            .collect::<Vec<String>>()
            .first()
            .unwrap_or(&Network::Bitcoin.to_string())
            .clone();
        InvoiceInfo {
            issuer_id: offer.issuer().map(|id| id.to_string()),
            amount_msat: offer.amount().map(|a| {
                if let Amount::Bitcoin { amount_msats } = a {
                    amount_msats.clone()
                } else {
                    unimplemented!()
                }
            }),
            network: network.to_string(),
            description: offer.description().map(|str| str.to_string()),
            routes: Vec::new(),
            hints: Vec::new(),
            expiry_time: offer.absolute_expiry().map(|a| a.as_millis() as u64),
        }
    } else {
        unreachable!()
    };

    Ok(json::to_value(&invoice)?)
}

pub fn json_pay(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `pay` with request `{:?}`", request);
    let request: Pay = json::from_value(request.clone())?;
    let events = ctx.handler().events();
    if let Ok(_) = offer::Offer::from_str(&request.invoice_str) {
        ctx.offchain_manager()
            .pay_offer(&request.invoice_str, request.amount)?;
    } else {
        ctx.offchain_manager()
            .pay_invoice(&request.invoice_str, request.amount)?;
    }
    // FIXME: this will loop when the Payment event is not generated
    loop {
        let event = events
            .recv_timeout(Duration::from_secs(30))
            // FIXME: this should be avoided, the `?` should be used here
            .map_err(|err| {
                Error::Rpc(RpcError {
                    code: -1,
                    message: format!("{err}"),
                    data: None,
                })
            })?;

        if let Event::Lightning(LightningEvent::PaymentEvent {
            payment_hash,
            path,
            state,
        }) = event
        {
            return Ok(json::to_value(PayResult {
                state,
                path,
                payment_hash,
            })?);
        }
    }
}

pub fn json_keysend(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::debug!("call for `keysend` with request `{:?}`", request);
    let request: KeySend = json::from_value(request.clone())?;
    ctx.offchain_manager()
        .keysend(request.destination, request.amount_msat)?;
    // FIXME: return a better response
    Ok(json::json!({}))
}
