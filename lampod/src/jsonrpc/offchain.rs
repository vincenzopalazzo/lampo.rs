//! Offchain RPC methods

use lampo_common::json;
use lampo_common::model::request::GenerateInvoice;
use lampo_common::model::response::Invoice;
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
