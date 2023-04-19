//! Peer Control JSON RPC Interface!

use lampo_common::json;
use lampo_common::model::Connect;
use tokio::runtime::Runtime;

use crate::{ln::events::PeerEvents, LampoDeamon};

pub fn json_connect(ctx: &LampoDeamon, request: &json::Value) -> json::Value {
    log::info!("call for `connect` with request `{:?}`", request);
    let input: Connect = json::from_value(request.clone()).unwrap();

    let rt = Runtime::new().unwrap();
    let result = rt.block_on(async {
        ctx.peer_manager()
            .connect(input.node_id(), input.addr())
            .await
    });
    result.unwrap();
    request.clone()
}
