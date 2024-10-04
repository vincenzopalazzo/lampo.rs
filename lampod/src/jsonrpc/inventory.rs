//! Inventory method implementation
use lampo_common::json;
use lampo_common::jsonrpc::Error;
use lampo_common::model::request::NetworkInfo;
use lampo_common::model::response::{NetworkChannel, NetworkChannels};
use lampo_common::model::GetInfo;

use crate::LampoDaemon;

// FIXME: change the name to `json_get_info`
pub fn json_getinfo(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("calling `getinfo` with request `{:?}`", request);
    let chain = ctx.conf.network.to_string();
    let alias = ctx.conf.alias.clone();
    // we have to put "" in case of alias missing as cln provide us with a random alias.
    let alias = alias.unwrap_or_default();
    // FIXME: blockheight should be fetched from the blockchain
    let blockheight = 0;
    let lampo_dir = ctx.conf.root_path.to_string();
    // We provide a vector here as there may be other types of address in future like tor and ipv6.
    let mut address_vec = Vec::new();
    let address = ctx.conf.announce_addr.clone();
    if let Some(addr) = address {
        let port = ctx.conf.port.clone();
        // For now we don't iterate as there is only one type of address.
        let address_info = NetworkInfo {
            address: addr,
            port,
        };
        address_vec.push(address_info);
    }
    let getinfo = GetInfo {
        node_id: ctx
            .channel_manager()
            .manager()
            .get_our_node_id()
            .to_string(),
        peers: ctx.peer_manager().manager().list_peers().len(),
        channels: ctx.channel_manager().manager().list_channels().len(),
        chain,
        alias,
        blockheight,
        lampo_dir,
        address: address_vec,
    };

    Ok(json::to_value(getinfo)?)
}

// FIXME: check the request
pub fn json_networkchannels(ctx: &LampoDaemon, _: &json::Value) -> Result<json::Value, Error> {
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
