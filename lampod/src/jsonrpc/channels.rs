use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json;
use lampo_common::jsonrpc::{Error, RpcError};
use lampo_common::model::request;
use lampo_common::model::response;
use tokio::time::timeout;

use crate::rpc_error;
use crate::LampoDaemon;

pub async fn json_channels(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `list_channels` with request {:?}", request);
    let resp = ctx.channel_manager().list_channels();
    Ok(json::to_value(resp)?)
}

pub async fn json_close(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `closechannel` with request {:?}", request);
    let close_request: request::CloseChannel = json::from_value(request.clone())?;
    let mut receiver = ctx.handler().events();

    // This gives all the channels with associated peer
    let channels: response::Channels = ctx
        .handler()
        .call(
            "channels",
            json::json!({
                "peer_id": close_request.node_id,
            }),
        )
        .await?;

    let res = if channels.channels.len() > 1 {
        // check the channel_id if it is not none, if it is return an error
        // and if it is not none then we need to have the channel_id that needs to be shut
        if close_request.channel_id.is_none() {
            return Err(rpc_error!("Channels > 1, provide `channel_id`"));
        } else {
            close_request
        }
    } else if !channels.channels.is_empty() {
        // This is the case where channel with the given node_id = 1
        // SAFETY: it is safe to unwrap because the channels is not empty
        let mut final_request = close_request.clone();
        final_request.channel_id = Some(channels.channels.first().unwrap().channel_id.clone());
        final_request
    } else {
        // No channels with the given peer.
        return Err(rpc_error!("No channels with associated peer"));
    };
    ctx.channel_manager().close_channel(res)?;

    // FIXME: would be good to have some sort of macros, because
    // this is a common patter across lampo
    let (message, channel_id, node_id, funding_utxo) = loop {
        match timeout(std::time::Duration::from_secs(30), receiver.recv()).await {
            Ok(Some(event)) => {
                if let Event::Lightning(LightningEvent::CloseChannelEvent {
                    message,
                    channel_id,
                    counterparty_node_id,
                    funding_utxo,
                }) = event
                {
                    break (message, channel_id, counterparty_node_id, funding_utxo);
                }
            }
            Ok(None) => {
                return Err(Error::Rpc(RpcError {
                    code: -1,
                    message: "Event channel closed while waiting for CloseChannelEvent".to_string(),
                    data: None,
                }));
            }
            Err(_) => {
                return Err(Error::Rpc(RpcError {
                    code: -1,
                    message: "Timeout while waiting for CloseChannelEvent".to_string(),
                    data: None,
                }));
            }
        }
    };

    // FIXME: wrap this under a struct
    Ok(json::json!({
        "message" : message,
        "channel_id" : channel_id,
        "peer_id" : node_id,
        "funding_utxo" : funding_utxo,
    }))
}
