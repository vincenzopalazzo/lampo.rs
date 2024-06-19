//! Inventory method implementation
use lampo_common::json;
use lampo_jsonrpc::errors::Error;

use crate::LampoDaemon;

pub fn get_info(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("calling `getinfo` with request `{:?}`", request);
    let result = ctx.call("getinfo", request.clone())?;
    Ok(result)
}
