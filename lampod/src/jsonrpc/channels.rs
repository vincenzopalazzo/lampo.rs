use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json;
use lampo_common::jsonrpc::{Error, RpcError};
use lampo_common::model::request;
use lampo_common::model::response;

use crate::rpc_error;
use crate::LampoDaemon;

pub async fn json_channels(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `list_channels` with request {:?}", request);
    let resp = ctx.channel_manager().list_channels();
    Ok(json::to_value(resp)?)
}

pub async fn json_close(ctx: &LampoDaemon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `closechannel` with request {:?}", request);
    let mut request: request::CloseChannel = json::from_value(request.clone())?;
    let mut events = ctx.handler().events();
    // This gives all the channels with associated peer
    let channels: response::Channels = ctx
        .handler()
        .call(
            "channels",
            json::json!({
                "peer_id": request.node_id,
            }),
        )
        .await?;

    let res = if channels.channels.len() > 1 {
        // check the channel_id if it is not none, if it is return an error
        // and if it is not none then we need to have the channel_id that needs to be shut
        if request.channel_id.is_none() {
            return Err(rpc_error!("Channels > 1, provide `channel_id`"));
        } else {
            request
        }
    } else if !channels.channels.is_empty() {
        // This is the case where channel with the given node_id = 1
        // SAFETY: it is safe to unwrap because the channels is not empty
        let channel = channels.channels.first().unwrap();
        request.channel_id = Some(channel.channel_id.clone());
        request
    } else {
        // No channels with the given peer.
        return Err(rpc_error!("No channels with associated peer"));
    };
    ctx.channel_manager().close_channel(res)?;

    // FIXME: would be good to have some sort of macros, because
    // this is a common patter across lampo
    let (message, channel_id, node_id, funding_utxo) = loop {
        let event = events
            .recv()
            .await
            .ok_or(Error::Rpc(RpcError {
                code: -1,
                message: format!("No event received"),
                data: None,
            }))
            // FIXME: find a way to map this error
            .map_err(|err| {
                Error::Rpc(RpcError {
                    code: -1,
                    message: format!("{err}"),
                    data: None,
                })
            })?;
        if let Event::Lightning(LightningEvent::CloseChannelEvent {
            message,
            channel_id,
            counterparty_node_id,
            funding_utxo,
        }) = event
        {
            break (message, channel_id, counterparty_node_id, funding_utxo);
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
