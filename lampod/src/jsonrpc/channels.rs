use lampo_common::json;
use lampo_jsonrpc::errors::Error;

use crate::LampoDeamon;

pub fn json_list_channels(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `list_channels` with request {:?}", request);
    let resp = ctx.channel_manager().list_channel();
    Ok(json::to_value(resp)?)
}
