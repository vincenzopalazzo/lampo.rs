//! JSON RPC 2.0 implementation
pub mod inventory;
pub mod onchain;
pub mod open_channel;
pub mod peer_control;

use std::cell::RefCell;
use std::sync::Arc;

use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::json;
use lampo_jsonrpc::command::Context;
use lampo_jsonrpc::json_rpc2;
use lampo_jsonrpc::Handler;

use crate::{handler::external_handler::ExternalHandler, LampoDeamon};

/// JSON RPC 2.0 Command handler!
pub struct CommandHandler {
    pub handler: RefCell<Option<Arc<Handler<LampoDeamon>>>>,
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

    pub fn set_handler(&self, handler: Arc<Handler<LampoDeamon>>) {
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
        let Some(resp) = handler.run_callback(&req) else {
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
impl Context for LampoDeamon {
    type Ctx = LampoDeamon;

    fn ctx(&self) -> &Self::Ctx {
        self
    }
}
