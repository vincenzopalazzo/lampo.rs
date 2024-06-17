//! Peer Control JSON RPC Interface!
use std::sync::Arc;

use lampo_common::json;
use lampo_common::model::Connect;

use crate::jsonrpc::Result;
use crate::ln::events::PeerEvents;
use crate::LampoDaemon;

pub async fn json_connect(ctx: Arc<LampoDaemon>, request: json::Value) -> Result<json::Value> {
    log::info!("call for `connect` with request `{:?}`", request);
    let input: Connect = json::from_value(request.clone())?;
    let host = input.addr()?;
    let node_id = input.node_id()?;
    let _ = ctx.peer_manager().connect(node_id, host).await?;
    Ok(request.clone())
}
