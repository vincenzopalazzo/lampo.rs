//! Open Channel RPC Method implementation
use lampo_common::json;
use lampo_common::model::request;

use crate::json_rpc2::{Error, RpcError};
use crate::ln::events::ChannelEvents;
use crate::LampoDaemon;

pub fn json_open_channel(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `openchannel` with request {:?}", request);
    let request: request::OpenChannel = json::from_value(request.clone())?;

    // LDK's `create_channel()` doesn't check if you are currently connected
    // to the given peer so we need to check ourselves
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
        let _ = ctx.rt.enter();
        ctx.call("connect", conn).map_err(|err| {
            Error::Rpc(RpcError {
                code: -1,
                message: format!("connect fails with: {err}"),
                data: None,
            })
        })?;
    }

    // FIXME: there are use case there need to be covered, like
    // - When there is an error how we return back to the user?
    // - In this case there is some feedback that ldk need to give us
    // before return the message, so we should design a solution for this.
    let resp = ctx.channel_manager().open_channel(request);
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
