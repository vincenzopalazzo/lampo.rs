//! Inventory method implementation
use std::sync::Arc;

use lampo_common::json;

use crate::jsonrpc::Result;
use crate::LampoDaemon;

pub async fn get_info(ctx: Arc<LampoDaemon>, request: json::Value) -> Result<json::Value> {
    log::info!("calling `getinfo` with request `{:?}`", request);
    let handler = ctx.handler();
    let result = handler.call("getinfo", request).await?;
    Ok(result)
}
