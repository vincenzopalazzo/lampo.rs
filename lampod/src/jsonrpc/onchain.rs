//! On Chain RPC methods
use lampo_common::json;
use lampo_jsonrpc::errors::{Error, RpcError};

use crate::LampoDeamon;

pub fn json_new_addr(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `new_addr` with request {:?}", request);
    let resp = ctx.wallet_manager().get_onchain_address();
    match resp {
        Ok(resp) => Ok(json::to_value(resp)?),
        Err(err) => Err(Error::Rpc(RpcError {
            code: -1,
            message: format!("{err}"),
            data: None,
        })),
    }
}

pub fn json_funds(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `funds` with request `{:?}`", request);
    match ctx.wallet_manager().list_transactions() {
        Ok(transactions) => Ok(json::json!({
            "transactions": transactions,
        })),
        Err(err) => Err(Error::Rpc(RpcError {
            code: -1,
            message: format!("{err}"),
            data: None,
        })),
    }
}
