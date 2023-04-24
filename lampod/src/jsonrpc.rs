//! JSON RPC 2.0 implementation
pub mod inventory;
pub mod peer_control;

use std::env;

use lampo_client::UnixClient;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::json;
use lampo_jsonrpc::command::Context;
use lampo_jsonrpc::json_rpc2;
use once_cell::sync::Lazy;

use crate::{handler::external_handler::ExternalHandler, LampoDeamon};

/// JSON RPC 2.0 Command handler!
pub struct CommandHandler {
    pub client: Lazy<UnixClient>,
    pub conf: LampoConf,
}

unsafe impl Send for CommandHandler {}
unsafe impl Sync for CommandHandler {}

impl CommandHandler {
    pub fn new(lampo_conf: &LampoConf) -> error::Result<Self> {
        let client: Lazy<UnixClient> = Lazy::new(|| {
            let path = env::var("LAMPO_UNIX").unwrap();
            UnixClient::new(&path).unwrap()
        });
        let handler = CommandHandler {
            client,
            conf: lampo_conf.clone(),
        };
        Ok(handler)
    }
}

impl ExternalHandler for CommandHandler {
    fn handle(&self, req: &json_rpc2::Request<json::Value>) -> error::Result<Option<json::Value>> {
        log::debug!("handling the JSON RPC response with req {:?}", req);
        let resp = self.client.call(&req.method, req.params.clone())?;
        // FIXME: we should manage the handler when we try to handle
        // a method that it is not supported by this handler
        //
        // Like we should look at the error code, and return None.
        Ok(resp)
    }
}

/// Implementing the Context for the JSON RPC 2.0 framework
impl Context for LampoDeamon {
    type Ctx = LampoDeamon;

    fn ctx(&self) -> &Self::Ctx {
        self
    }
}
