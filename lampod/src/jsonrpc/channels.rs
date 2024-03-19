use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::json;
use lampo_common::model::request;
use lampo_jsonrpc::errors::Error;
use lampo_jsonrpc::errors::RpcError;
use lampo_common::handler::Handler;

use crate::ln::events::ChannelEvents;

use crate::LampoDeamon;

pub fn json_list_channels(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `list_channels` with request {:?}", request);
    let resp = ctx.channel_manager().list_channel();
    Ok(json::to_value(resp)?)
}

pub fn json_close_channel(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `closechannel` with request {:?}", request);
    let request: request::CloseChannel = json::from_value(request.clone())?;
    let events = ctx.handler().events();
    let res = ctx.channel_manager().close_channel(request);
    let (message, channel_id, node_id, funding_utxo) = loop {
        let event = events.recv_timeout(std::time::Duration::from_secs(30)).map_err(|err| {
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
    let resp = match res {
        Ok(_) => Ok(json::json!({
            "message" : message,
            "channel_id" : channel_id,
            "counterparty_node_id" : node_id,
            "funding_utxo" : funding_utxo,
        })),
        Err(err) => Err(Error::Rpc(RpcError {
            code: -1,
            message: format!("{err}"),
            data: None,
        })),
    };
    Ok(json::to_value(resp?)?)
}
