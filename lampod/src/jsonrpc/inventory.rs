//! Inventory method implementation
use lampo_common::chan;
use lampo_common::error;
use lampo_common::json;

use crate::json_rpc2::Error;
use crate::LampoDaemon;

pub async fn get_info(ctx: &LampoDaemon, request: json::Value) -> Result<json::Value, Error> {
    log::info!("calling `getinfo` with request `{:?}`", request);
    let handler = ctx.handler();
    let (inchan, outchan) = chan::unbounded::<error::Result<json::Value>>();

    // FIXME: We should not call clone here! but this is still working in progress so for not it is ok.
    let request = request.clone();
    tokio::spawn(async move {
        let result = handler.call("getinfo", request.clone()).await;
        inchan.send(result);
    });
    // FIXME: we should not unwrap the chain error.
    let result = outchan.recv().unwrap()?;
    Ok(result)
}
