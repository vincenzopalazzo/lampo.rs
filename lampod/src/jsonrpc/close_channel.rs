//! Open Channel RPC Method implementation

use lampo_common::json;
use lampo_common::model::request;
use lampo_jsonrpc::errors::{Error, RpcError};

use crate::ln::events::ChannelEvents;
use crate::LampoDeamon;

pub fn json_close_channel(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `closechannel` with request {:?}", request);
    let request: request::CloseChannel = json::from_value(request.clone())?;
    let resp = ctx.channel_manager().close_channel(request);
    let resp = match resp {
        Ok(resp) => Ok(resp),
        Err(err) => Err(Error::Rpc(RpcError {
            code: -1,
            message: format!("{err}"),
            data: None,
        })),
    };
    Ok(json::to_value(resp?)?)
}
