//! Inventory method implementation
use lampo_common::json;
use lampo_jsonrpc::errors::{Error, RpcError};

use crate::LampoDaemon;

pub fn get_info(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("calling `getinfo` with request `{:?}`", request);
    let result = ctx.call("getinfo", request.clone());
    let Ok(result) = result else {
        let err = RpcError {
            message: format!("command error {:?}", request),
            code: -1,
            data: None,
        };
        return Err(Error::Rpc(err));
    };
    Ok(result)
}
