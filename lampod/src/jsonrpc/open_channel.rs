//! Open Channel RPC Method implementation
use tokio::runtime::Runtime;

use lampo_common::json;
use lampo_common::model::request;
use lampo_jsonrpc::errors::{Error, RpcError};

use crate::{ln::events::ChannelEvents, LampoDeamon};

pub fn json_open_channel(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `openchannel` with request {:?}", request);
    let request: request::OpenChannel = json::from_value(request.clone())?;
    // FIXME: remove unwrap!
    if !ctx
        .peer_manager()
        .is_connected_with(request.node_id().unwrap())
    {
        log::trace!("we are not connected with the peer {}", request.node_id);
        let conn = request::Connect::try_from(request.clone()).map_err(|err| {
            Error::Rpc(RpcError {
                code: -1,
                message: format!("{err}"),
                data: None,
            })
        })?;
        let conn = json::to_value(conn)?;
        let handler = Runtime::new()?;
        handler.block_on(ctx.call("connect", conn)).map_err(|err| {
            Error::Rpc(RpcError {
                code: -1,
                message: format!("connect fails with: {err}"),
                data: None,
            })
        })?;
    }
    let resp = ctx.channel_manager().open_channel(request);
    match resp {
        Ok(resp) => Ok(json::to_value(resp)?),
        Err(err) => Err(Error::Rpc(RpcError {
            code: -1,
            message: format!("{err}"),
            data: None,
        })),
    }
}
