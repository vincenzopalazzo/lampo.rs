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
    // Issue #111: a peer that is ahead of an IBD bitcoind makes funding
    // impossible. Refuse before `create_channel` so we do not leave a
    // temporary channel that can never confirm.
    if ctx.chain_sync().backend_syncing() {
        return Err(crate::rpc_error!(
            "bitcoind is still syncing; refusing to fund a channel until the backend catches up"
        ));
    }
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
        crate::jsonrpc::peer_control::json_connect(ctx, &conn).await?;
    }

    // Surface the funding error itself. `open_channel` waits on the event
    // bus; a fee or wallet failure used to be logged and then dropped, so
    // this RPC returned only after the receive timed out (issues #221 / #237).
    let resp = ctx
        .channel_manager()
        .open_channel(request)
        .await
        .map_err(|err| {
            log::warn!(
                target: "lampod::jsonrpc::open_channel",
                "fundchannel failed for peer {node_id}: {err}"
            );
            crate::rpc_error!("{err}")
        })?;
    Ok(json::to_value(resp)?)
}
