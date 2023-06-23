//! On Chain RPC methods
use lampo_common::json;
use lampo_jsonrpc::errors::{Error, RpcError};

use crate::LampoDeamon;

pub fn json_new_addr(ctx: &LampoDeamon, request: &json::Value) -> Result<json::Value, Error> {
    log::info!("call for `openchannel` with request {:?}", request);
    let resp = ctx.wallet_manager().get_onchain_address();
    match resp {
        Ok(resp) => Ok(json::to_value(resp)?),
        Err(err) => Err(Error::Rpc(RpcError {
            code: -1,
            message: format!("{err}"),
            data: None,
        })),
    }
}
