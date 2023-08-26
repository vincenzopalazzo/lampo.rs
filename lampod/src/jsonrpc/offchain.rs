//! Offchain RPC methods

use lampo_common::ldk;
use lampo_common::model::request::GenerateInvoice;
use lampo_common::model::request::KeySend;
use lampo_common::model::response::{Invoice, InvoiceInfo};
use lampo_common::{json, model::request::DecodeInvoice};
use lampo_jsonrpc::errors::{Error, RpcError};

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
    let request: DecodeInvoice = json::from_value(request.clone())?;
    ctx.offchain_manager()
        .pay_invoice(&request.invoice_str, None)
        .map_err(|err| {
            Error::Rpc(RpcError {
                code: -1,
                message: format!("{err}"),
                data: None,
            })
        })?;

    Ok(json::json!({}))
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
