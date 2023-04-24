//! Peer Control JSON RPC Interface!
use tokio::runtime::Runtime;

use lampo_common::json;
use lampo_common::model::Connect;
use lampo_jsonrpc::errors::Error;

use crate::{ln::events::PeerEvents, LampoDeamon};

pub fn json_connect(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `connect` with request `{:?}`", request);
    let input: Connect = json::from_value(request.clone()).unwrap();

    let rt = Runtime::new().unwrap();
    // FIXME: return a better result
    let _ = rt
        .block_on(async {
            ctx.peer_manager()
                .connect(input.node_id(), input.addr())
                .await
        })
        .unwrap();
    Ok(request.clone())
}
