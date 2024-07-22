//! Inventory method implementation
use lampo_common::json;
use lampo_common::model::response::{NetworkChannel, NetworkChannels};
use lampo_jsonrpc::errors::Error;

use crate::LampoDaemon;

pub fn get_info(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("calling `getinfo` with request `{:?}`", request);
    let result = ctx.call("getinfo", request.clone())?;
    Ok(result)
}

// FIXME: check the request
pub fn json_network_channels(ctx: &LampoDaemon, _: &json::Value) -> Result<json::Value, Error> {
    let network_graph = ctx.channel_manager().graph();
    let network_graph = network_graph.read_only();
    let channels = network_graph.channels().unordered_keys();
    let mut network_channels = Vec::new();
    for short_id in channels {
        let Some(channel) = network_graph.channel(*short_id) else {
            continue;
        };
        network_channels.push(NetworkChannel::from(channel.clone()));
    }
    Ok(json::to_value(NetworkChannels {
        channels: network_channels,
    })?)
}
