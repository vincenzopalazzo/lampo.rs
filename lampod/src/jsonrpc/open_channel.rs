//! Open Channel RPC Method implementation
use lampo_common::json;
use lampo_common::jsonrpc::Result;
use lampo_common::model::request;

use crate::ln::events::ChannelEvents;
use crate::LampoDaemon;

pub fn json_open_channel(ctx: &LampoDaemon, request: json::Value) -> Result<json::Value> {
    log::info!("call for `openchannel` with request {:?}", request);
    let inn_request: request::OpenChannel = json::from_value(request.clone())?;

    // LDK's `create_channel()` doesn't check if you are currently connected
    // to the given peer so we need to check ourselves
    // FIXME: remove unwrap!
    if !ctx
        .peer_manager()
        .is_connected_with(inn_request.node_id().unwrap())
    {
        log::trace!("we are not connected with the peer {}", inn_request.node_id);
        let conn = request::Connect::try_from(inn_request.clone())?;
        let conn = json::to_value(conn)?;
        ctx.call("connect", conn)?;
        json_open_channel(ctx, request.clone())?;
    }

    // FIXME: there are use case there need to be covered, like
    // - When there is an error how we return back to the user?
    // - In this case there is some feedback that ldk need to give us
    // before return the message, so we should design a solution for this.
    let resp = ctx.channel_manager().open_channel(inn_request)?;
    Ok(json::to_value(resp)?)
}
