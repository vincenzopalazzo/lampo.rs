//! Open Channel RPC Method implementation

use lampo_common::json;
use lampo_common::jsonrpc::{Error, RpcError};
use lampo_common::model::request;

use crate::LampoDaemon;

pub async fn json_fundchannel(
    ctx: &LampoDaemon,
    request: &json::Value,
) -> Result<json::Value, Error> {
    log::info!("call for `openchannel` with request {:?}", request);
    let request: request::OpenChannel = json::from_value(request.clone())?;

    // LDK's `create_channel()` doesn't check if you are currently connected
    // to the given peer so we need to check ourselves
    //
    // A malformed `node_id` must surface as an RPC error, not panic the
    // actix worker: found by the regtest soak simulation, where a caller
    // sending `"node_id": ""` killed the worker thread
    // (`called Result::unwrap() on an Err value: malformed public key`).
    let node_id = request
        .node_id()
        .map_err(|err| crate::rpc_error!("invalid `node_id` provided: {err}"))?;
    if !ctx.peer_manager().is_connected_with(node_id) {
        log::trace!("we are not connected with the peer {}", request.node_id);
        let conn = request::Connect::try_from(request.clone())?;
        let conn = json::to_value(conn)?;
        ctx.call("connect", conn).await?;
    }

    // FIXME: there are use case there need to be covered, like
    // - When there is an error how we return back to the user?
    // - In this case there is some feedback that ldk need to give us
    // before return the message, so we should design a solution for this.
    let resp = ctx.channel_manager().open_channel(request).await?;
    Ok(json::to_value(resp)?)
}
