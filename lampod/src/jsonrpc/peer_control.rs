//! Peer Control JSON RPC Interface!
use lampo_common::json;
use lampo_common::model::Connect;
use lampo_jsonrpc::errors::Error;
use lampo_jsonrpc::errors::RpcError;

use crate::{ln::events::PeerEvents, LampoDeamon};

pub fn json_connect(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `connect` with request `{:?}`", request);
    let input: Connect = json::from_value(request.clone())?;

    let _ = ctx
        .rt
        .block_on(async {
            ctx.peer_manager()
                .connect(input.node_id(), input.addr())
                .await
        })
        .map_err(|err| RpcError {
            code: -1,
            message: format!("{err}"),
            data: None,
        })?;
    Ok(request.clone())
}
