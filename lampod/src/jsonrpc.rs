//! JSON RPC 2.0 implementation
pub mod channels;
pub mod inventory;
pub mod offchain;
pub mod onchain;
pub mod open_channel;
pub mod peer_control;

use std::cell::RefCell;
use std::sync::Arc;

use lampo_async_jsonrpc::command::Context;
use lampo_async_jsonrpc::json_rpc2;
use lampo_async_jsonrpc::Handler;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::json;

use crate::{handler::external_handler::ExternalHandler, LampoDaemon};

#[macro_export]
macro_rules! rpc_error {
    ($($msg:tt)*) => {{
        Error::Rpc(RpcError {
            code: -1,
            message: format!($($msg)*),
            data: None,
        })
    }};
}

/// JSON RPC 2.0 Command handler!
pub struct CommandHandler {
    pub handler: RefCell<Option<Arc<Handler<LampoDaemon>>>>,
    pub conf: LampoConf,
}

unsafe impl Send for CommandHandler {}
unsafe impl Sync for CommandHandler {}

impl CommandHandler {
    pub fn new(lampo_conf: &LampoConf) -> error::Result<Self> {
        let handler = CommandHandler {
            handler: RefCell::new(None),
            conf: lampo_conf.clone(),
        };
        Ok(handler)
    }

    pub fn set_handler(&self, handler: Arc<Handler<LampoDaemon>>) {
        self.handler.replace(Some(handler));
    }
}

impl ExternalHandler for CommandHandler {
    fn handle(&self, req: &json_rpc2::Request<json::Value>) -> error::Result<Option<json::Value>> {
        let handler = self.handler.borrow();
        let Some(handler) = handler.as_ref() else {
            log::info!("skipping the handling because it is not defined");
            return Ok(None);
        };
        log::debug!("handling the JSON RPC response with req {:?}", req);
        // FIXME: store the ctx inside the handler and not take as argument!
        let Some(resp) = handler.run_callback(req) else {
            log::info!("callback `{}` not found, skipping handler", req.method);
            return Ok(None);
        };
        // FIXME: we should manage the handler when we try to handle
        // a method that it is not supported by this handler
        //
        // Like we should look at the error code, and return None.
        Ok(Some(resp?))
    }
}

/// Implementing the Context for the JSON RPC 2.0 framework
impl Context for LampoDaemon {
    type Ctx = LampoDaemon;

    fn ctx(&self) -> &Self::Ctx {
        self
    }
}
