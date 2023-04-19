//! Peer Control JSON RPC Interface!

use lampo_common::json;
use lampo_common::model::Connect;
use tokio::runtime::Handle;

use crate::{ln::events::PeerEvents, LampoDeamon};

pub fn json_connect(ctx: &LampoDeamon, request: &json::Value) -> json::Value {
    let input: Connect = json::from_value(request.clone()).unwrap();

    let handler = Handle::current();
    let _ = handler.enter();
    let result = futures::executor::block_on(async {
        ctx.peer_manager()
            .connect(input.node_id(), input.addr())
            .await
    });
    result.unwrap();
    request.clone()
}
