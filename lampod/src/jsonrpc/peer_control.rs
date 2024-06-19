//! Peer Control JSON RPC Interface!
use lampo_common::json;
use lampo_common::model::Connect;
use lampo_jsonrpc::errors::Error;

use crate::{ln::events::PeerEvents, LampoDaemon};

pub fn json_connect(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `connect` with request `{:?}`", request);
    let input: Connect = json::from_value(request.clone())?;
    let host = input.addr()?;
    let node_id = input.node_id()?;

    ctx.rt.block_on(ctx.peer_manager().connect(node_id, host))?;
    Ok(request.clone())
}
