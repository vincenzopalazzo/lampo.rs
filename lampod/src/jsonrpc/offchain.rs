//! Offchain RPC methods
use std::borrow::Borrow;
use std::borrow::BorrowMut;
use std::str::FromStr;
use std::time::Duration;

use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::ldk;
use lampo_common::ldk::offers::offer;
use lampo_common::model::request::GenerateInvoice;
use lampo_common::model::request::GenerateOffer;
use lampo_common::model::request::KeySend;
use lampo_common::model::request::Pay;
use lampo_common::model::response;
use lampo_common::model::response::PayResult;
use lampo_common::model::response::{Invoice, InvoiceInfo};
use lampo_common::{json, model::request::DecodeInvoice};
use lampo_jsonrpc::errors::{Error, RpcError};
use lampo_common::error;
use lampo_common::ldk::ln::channelmanager::PaymentId;
use lampo_common::ldk::ln::channelmanager::Retry;
use lampo_common::ldk::invoice::payment;
use lampo_common::btc::bitcoin::hashes::Hash;


use crate::rpc_error;
use crate::LampoDaemon;

#[cfg(feature = "vanilla")]
pub fn json_invoice(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
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

//Bolt12 not inside v0.0.118?
#[cfg(feature = "vanilla")]
pub fn json_offer(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `offer` with request `{:?}`", request);
    let request: GenerateOffer = json::from_value(request.clone())?;
    let manager = ctx.channel_manager().manager();
    let mut offer_builder = manager
        .create_offer_builder()
        .map_err(|err| crate::rpc_error!("{:?}", err))?
        .description(request.description);

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

#[cfg(feature = "vanilla")]
pub fn json_pay(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
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

struct RgbPayVisitor;

impl RgbPayVisitor {
    fn new() -> RgbPayVisitor { RgbPayVisitor }
}

struct VanillaPayVisitor;

impl VanillaPayVisitor {
    fn new() -> VanillaPayVisitor { VanillaPayVisitor }
}

pub trait LampoVisitor {
    // We need two different function signature of pay_invoice as rgb lightning also require
    // channel_manager as an argument
    fn pay_invoice(&self, ctx: LampoDaemon, invoice_str: &str, amount_msat: Option<u64>) -> error::Result<()>;
}

pub trait PayTrait<T: LampoVisitor> {
    fn json_pay(&self, ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error>;
}

// Pay invoice for rgb
#[cfg(feature = "rgb")]
impl LampoVisitor for RgbPayVisitor {
    fn pay_invoice(&self, ctx: LampoDaemon, invoice_str: &str, amount_msat: Option<u64>) -> error::Result<()> {
        let invoice = ctx.offchain_manager.unwrap().decode_invoice(invoice_str)?;
        let payment_id = PaymentId((*invoice.payment_hash()).into_inner());
        let channel_manager = ctx.channel_manager.unwrap();
        let res = match payment::pay_invoice(
            &invoice,
            Retry::Timeout(Duration::from_secs(10)),
            channel_manager,
        ) {
            Ok(_payment_id) => {
                let payee_pubkey = invoice.recover_payee_pub_key();
                let amt_msat = invoice.amount_milli_satoshis().unwrap();
                log::info!(
                    "EVENT: initiated sending {} msats to {}",
                    amt_msat,
                    payee_pubkey
                );
                Ok(())
            }
            Err(e) => {
                log::error!("Failed to send payment: {:?}", e);
                Ok(())
            }
        };
        Ok(())
    }
}

pub struct RgbPayDispatch<T: LampoVisitor>{
    visitor: T,
}

pub struct VanillaPayDispatch<T: LampoVisitor> {
    visitor: T,
}

impl<T: LampoVisitor> PayTrait<T> for RgbPayDispatch<T> {
    fn json_pay(&self, ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
        todo!()
    }
}

#[cfg(feature = "vanilla")]
impl<T: LampoVisitor> PayTrait<T> for VanillaPayDispatch<T> {
    fn json_pay(&self, ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
        ctx.offchain_manager.pay_invoice();
        log::info!("call for `pay` with request `{:?}`", request);
        let request: Pay = json::from_value(request.clone())?;
        let events = ctx.handler().events();
        if let Ok(_) = offer::Offer::from_str(&request.invoice_str) {
            ctx.offchain_manager()
                .pay_offer(&request.invoice_str, request.amount)
                .map_err(|err| rpc_error!("{err}"))?;
        } else {
            // FIXME: This needs to change?
            // For now the VanillaPayDispatch will just use the pay_invoice as written in offchain_manager
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
}

#[cfg(feature = "vanilla")]
pub fn json_keysend(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
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
