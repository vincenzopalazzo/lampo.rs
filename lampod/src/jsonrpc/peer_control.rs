//! Peer Control JSON RPC Interface!
use lampo_common::json;
use lampo_common::model::Connect;

use crate::json_rpc2::Error;
use crate::json_rpc2::RpcError;
use crate::ln::events::PeerEvents;
use crate::LampoDaemon;

pub fn json_connect(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `connect` with request `{:?}`", request);
    let input: Connect = json::from_value(request.clone())?;
    let host = input.addr();
    let node_id = match input.node_id() {
        Err(err) => {
            let rpc_error = Error::Rpc(RpcError {
                code: -1,
                message: format!("{err}"),
                data: None,
            });
            return Err(rpc_error);
        }
        Ok(id) => id,
    };
    let result = match host {
        Err(err) => {
            let rpc_error = Error::Rpc(RpcError {
                code: -1,
                message: format!("{err}"),
                data: None,
            });
            Err(rpc_error)
        }
        Ok(host_value) => {
            let _ = ctx.peer_manager().connect(node_id, host_value);
            Ok(request.clone())
        }
    };
    result
}
