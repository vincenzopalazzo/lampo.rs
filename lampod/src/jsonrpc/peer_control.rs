//! Peer Control JSON RPC Interface!
use lampo_common::json;
use lampo_common::jsonrpc::Result;
use lampo_common::model::Connect;

use crate::LampoDaemon;

pub fn json_connect(ctx: &LampoDaemon, request: json::Value) -> Result<json::Value> {
    log::info!("call for `connect` with request `{:?}`", request);
    let input: Connect = json::from_value(request.clone())?;
    let host = input.addr()?;
    let node_id = input.node_id()?;

    let peer_manager = ctx.peer_manager();
    peer_manager.connect(node_id, host)?;
    Ok(request.clone())
}
