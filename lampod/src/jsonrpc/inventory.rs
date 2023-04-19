//! Inventory method implementation
use lampo_common::{json, model::GetInfo};
use lampo_jsonrpc::command::Context;

use crate::LampoDeamon;

pub fn get_info(ctx: &LampoDeamon, request: &json::Value) -> json::Value {
    log::info!("calling `getinfo` with request `{:?}`", request);
    let lampo = ctx.ctx();
    let getinfo = GetInfo {
        node_id: lampo
            .channel_manager()
            .manager()
            .get_our_node_id()
            .to_string(),
        peers: lampo.peer_manager().manager().get_peer_node_ids().len(),
        channels: 0,
    };
    json::to_value(getinfo).unwrap()
}
