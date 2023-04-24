//! Inventory method implementation
use tokio::runtime::Runtime;

use lampo_common::json;
use lampo_jsonrpc::errors::{Error, RpcError};

use crate::LampoDeamon;

pub fn get_info(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("calling `getinfo` with request `{:?}`", request);
    let rt = Runtime::new().unwrap();
    let result = rt.block_on(ctx.call("getinfo", request.clone()));
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
