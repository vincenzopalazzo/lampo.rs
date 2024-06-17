//! External Handler are the core part of the Lampo Implementation
//! becauuse it can be really anythings.
use async_trait::async_trait;

use lampo_common::error;
use lampo_common::json;

use crate::json_rpc2::Request;

#[async_trait]
pub trait ExternalHandler {
    async fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>>;
}
