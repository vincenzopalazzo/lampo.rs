//! Inventory method implementation
use tokio::runtime::Runtime;

use lampo_common::json;

use crate::LampoDeamon;

pub fn get_info(ctx: &LampoDeamon, request: &json::Value) -> json::Value {
    log::info!("calling `getinfo` with request `{:?}`", request);
    let rt = Runtime::new().unwrap();
    let result = rt.block_on(ctx.call("getinfo", request.clone()));
    let Ok(result) = result else {
        panic!("we must implement the rpc with error");
    };
    result
}
