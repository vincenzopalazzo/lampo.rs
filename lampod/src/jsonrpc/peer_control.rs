//! Peer Control JSON RPC Interface!
use lampo_common::json;
use lampo_common::jsonrpc::Error;
use lampo_common::model::Connect;

use crate::LampoDaemon;

pub async fn json_connect(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `connect` with request `{:?}`", request);
    let input: Connect = json::from_value(request.clone())?;
    let host = input.addr()?;
    let node_id = input.node_id()?;

    ctx.peer_manager().connect(node_id, host).await?;
    Ok(request.clone())
}
