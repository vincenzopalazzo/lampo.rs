//! On Chain RPC methods
use lampo_common::json;
use lampo_jsonrpc::errors::Error;

use crate::LampoDaemon;

pub fn json_new_addr(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `new_addr` with request {:?}", request);
    let resp = ctx.wallet_manager().get_onchain_address()?;
    Ok(json::to_value(resp)?)
}

pub fn json_funds(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `funds` with request `{:?}`", request);
    let txs = ctx.wallet_manager().list_transactions()?;
    Ok(json::json!({
        "transactions": txs,
    }))
}

pub fn json_estimate_fees(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `estimate_fees` with request `{:?}`", request);
    let response = ctx.onchain_manager().estimated_fees();
    Ok(json::to_value(response)?)
}
