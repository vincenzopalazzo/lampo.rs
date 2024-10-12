//! External Handler are the core part of the Lampo Implementation
//! becauuse it can be really anythings.

use lampo_common::{error, json};
use lampo_jsonrpc::json_rpc2::Request;

pub trait ExternalHandler {
    fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>>;
}
