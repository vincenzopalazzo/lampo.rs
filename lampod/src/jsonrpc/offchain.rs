//! Offchain RPC methods
use std::str::FromStr;
use std::time::Duration;

#[cfg(feature = "vanilla")] 
pub use {
    lampo_common::event::ln::LightningEvent,
    lampo_common::event::Event,
    lampo_common::handler::Handler,
    lampo_common::ldk,
    lampo_common::ldk::offers::offer,
    lampo_common::model::request::GenerateInvoice,
    lampo_common::model::request::GenerateOffer,
    lampo_common::model::request::KeySend,
    lampo_common::model::request::Pay,
    lampo_common::model::response,
    lampo_common::model::response::PayResult,
    lampo_common::model::response::{Invoice, InvoiceInfo},
    lampo_common::{json, model::request::DecodeInvoice},
    lampo_jsonrpc::errors::{Error, RpcError},
};

#[cfg(feature = "rgb")] 
pub use {
    rgb_lampo_common::event::ln::LightningEvent,
    rgb_lampo_common::event::Event,
    rgb_lampo_common::handler::Handler,
    rgb_lampo_common::ldk,
    rgb_lampo_common::ldk::offers::offer,
    rgb_lampo_common::model::request::GenerateInvoice,
    rgb_lampo_common::model::request::GenerateOffer,
    rgb_lampo_common::model::request::KeySend,
    rgb_lampo_common::model::request::Pay,
    rgb_lampo_common::model::response,
    rgb_lampo_common::model::response::PayResult,
    rgb_lampo_common::model::response::{Invoice, InvoiceInfo},
    rgb_lampo_common::{json, model::request::DecodeInvoice},
    lampo_jsonrpc::errors::{Error, RpcError},
};

use crate::rpc_error;
use crate::LampoDeamon;

pub fn json_invoice(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `invoice` with request `{:?}`", request);
    let request: GenerateInvoice = json::from_value(request.clone())?;
    let invoice = ctx
        .offchain_manager()
        .generate_invoice(
            request.amount_msat,
            &request.description,
            request.expiring_in.unwrap_or(10000),
        )
        .map_err(|err| {
            Error::Rpc(RpcError {
                code: -1,
                message: format!("{err}"),
                data: None,
            })
        })?;
    let invoice = Invoice {
        bolt11: invoice.to_string(),
    };
    Ok(json::to_value(&invoice)?)
}

pub fn json_offer(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `offer` with request `{:?}`", request);
    let request: GenerateOffer = json::from_value(request.clone())?;
    let manager = ctx.channel_manager().manager();
    let mut offer_builder = manager
        .create_offer_builder(request.description)
        .map_err(|err| crate::rpc_error!("{:?}", err))?;

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

pub fn json_decode_invoice(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `invoice` with request `{:?}`", request);
    let request: DecodeInvoice = json::from_value(request.clone())?;
    let invoice = ctx
        .offchain_manager()
        .decode_invoice(&request.invoice_str)
        .map_err(|err| {
            Error::Rpc(RpcError {
                code: -1,
                message: format!("{err}"),
                data: None,
            })
        })?;
    let invoice = InvoiceInfo {
        amount_msa: invoice.amount_milli_satoshis(),
        network: invoice.network().to_string(),
        description: match invoice.description() {
            ldk::invoice::Bolt11InvoiceDescription::Direct(dec) => dec.to_string(),
            ldk::invoice::Bolt11InvoiceDescription::Hash(_) => {
                "description hash provided".to_string()
            }
        },
        routes: Vec::new(),
        hints: Vec::new(),
        expiry_time: invoice.expiry_time().as_millis() as u64,
    };
    Ok(json::to_value(&invoice)?)
}

pub fn json_pay(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `pay` with request `{:?}`", request);
    let request: Pay = json::from_value(request.clone())?;
    let events = ctx.handler().events();
    if let Ok(_) = offer::Offer::from_str(&request.invoice_str) {
        ctx.offchain_manager()
            .pay_offer(&request.invoice_str, request.amount)
            .map_err(|err| rpc_error!("{err}"))?;
    } else {
        ctx.offchain_manager()
            .pay_invoice(&request.invoice_str, request.amount)
            .map_err(|err| rpc_error!("{err}"))?;
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

pub fn json_keysend(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::debug!("call for `keysend` with request `{:?}`", request);
    let request: KeySend = json::from_value(request.clone())?;
    ctx.offchain_manager()
        .keysend(request.destination, request.amount_msat)
        .map_err(|err| {
            Error::Rpc(RpcError {
                code: -1,
                message: format!("{err}"),
                data: None,
            })
        })?;
    Ok(json::json!({}))
}
