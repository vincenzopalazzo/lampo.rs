//! Peer Control JSON RPC Interface!
use lampo_common::json;
use lampo_common::model::Connect;
use lampo_jsonrpc::errors::Error;
use lampo_jsonrpc::errors::RpcError;

use crate::{ln::events::PeerEvents, LampoDeamon};

pub fn json_connect(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `connect` with request `{:?}`", request);
    let input: Connect = json::from_value(request.clone())?;
    let host = input.addr()?;
    let node_id = input.node_id()?;
    ctx.rt
        .block_on(ctx.peer_manager().connect(node_id, host))
        .map_err(|err| RpcError {
            code: -1,
            message: format!("{err}"),
            data: None,
        })?;
    Ok(request.clone())
}
