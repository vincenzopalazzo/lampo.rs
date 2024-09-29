/// Rest RPC module written with elite RPC.
///
/// Author: Vincenzo Palazzo <vincenzopalazzodev@gmail.com>
use std::io::Cursor;

use elite_rpc::protocol::Protocol;

use lampo_common::error;
use lampo_common::json;
use lampo_common::json::{Deserialize, Serialize};

#[derive(Clone)]
pub struct RestProtocol;

#[derive(Serialize, Deserialize)]
pub struct JsonResult {
    result: json::Value,
}

impl Protocol for RestProtocol {
    type InnerType = json::Value;

    fn new() -> error::Result<Self> {
        Ok(Self)
    }

    fn to_request(
        &self,
        url: &str,
        req: &Self::InnerType,
    ) -> error::Result<(String, Self::InnerType)> {
        Ok((url.to_string(), req.clone()))
    }

    fn from_request(
        &self,
        content: &[u8],
        _: std::option::Option<elite_rpc::protocol::Encoding>,
    ) -> error::Result<<Self as elite_rpc::protocol::Protocol>::InnerType> {
        let cursor = Cursor::new(content);
        let response: JsonResult = json::from_reader(cursor)?;
        Ok(response.result)
    }
}
