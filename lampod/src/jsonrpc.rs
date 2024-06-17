//! JSON RPC 2.0 implementation
pub mod channels;
pub mod inventory;
pub mod offchain;
pub mod onchain;
pub mod open_channel;
pub mod peer_control;

use std::cell::RefCell;
use std::sync::Arc;

use async_trait::async_trait;

use lampo_async_jsonrpc::{ErrorObject, IntoResponse, ResponsePayload};
use lampo_common::conf::LampoConf;
use lampo_common::json;
use lampo_common::json::{Deserialize, Serialize};
use lampo_common::{chan, error};

use crate::handler::external_handler::ExternalHandler;
use crate::LampoDaemon;

#[macro_export]
macro_rules! rpc_error {
    ($($msg:tt)*) => {{
        RpcError {
            code: -1,
            message: format!($($msg)*),
            data: None,
        }
    }};
}

pub type Result<T> = std::result::Result<T, RpcError>;

/// Trait to implement a specific API handler.
///
/// Immagine that there is a default API handler ship by lampo that you do
/// not like, and you would like to implement your own, so with this you can.
///
/// In addition, an handler can be also a plugin system that allow dynamically
/// added plugins.
#[async_trait]
pub trait Handler<T>: Sync + Send {
    /// Custom handler to handler a specific metod
    async fn handle(&self, request: &Request<json::Value>) -> Option<error::Result<json::Value>>;
}

/// JSON RPC 2.0 Command handler!
pub struct CommandHandler {
    pub handler: RefCell<Option<Arc<dyn Handler<LampoDaemon>>>>,
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

    pub fn add_handler(&self, handler: Arc<dyn Handler<LampoDaemon>>) {
        self.handler.replace(Some(handler));
    }
}

#[async_trait]
impl ExternalHandler for CommandHandler {
    async fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>> {
        // FIXME: remove clone
        let handler = self.handler.clone().into_inner();
        let Some(handler) = handler else {
            log::info!("skipping the handling because it is not defined");
            return Ok(None);
        };
        log::debug!("handling the JSON RPC response with req {:?}", req);
        // FIXME: store the ctx inside the handler and not take as argument!
        let Some(resp) = handler.handle(req).await else {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Str(String),
    Int(u16),
}

impl From<&str> for Id {
    fn from(value: &str) -> Self {
        Id::Str(value.to_owned())
    }
}

impl From<u64> for Id {
    fn from(value: u64) -> Self {
        Id::Str(format!("{value}"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
/// A standard JSONRPC request object
pub struct Request<T: Serialize> {
    /// The name of the RPC method call
    pub method: String,
    /// Parameters to the RPC method call
    pub params: T,
    /// Identifier for this Request, which should appear in the response
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    /// jsonrpc field, MUST be "2.0"
    pub jsonrpc: String,
}

impl<T: Serialize> Request<T> {
    pub fn new(method: &str, args: T) -> Self {
        Request {
            method: method.to_owned(),
            params: args,
            id: Some("lampo/jsonrpc/1".into()),
            jsonrpc: "2.0".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
/// A standard JSONRPC response object
pub struct Response<T> {
    /// A result if there is one, or null
    pub result: Option<T>,
    /// An error if there is one, or null
    pub error: Option<RpcError>,
    /// Identifier for this Request, which should match that of the request
    pub id: Option<Id>,
    /// jsonrpc field, MUST be "2.0"
    pub jsonrpc: String,
}

#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
/// A JSONRPCv2.0 spec compilant error object
pub struct RpcError {
    /// The integer identifier of the error
    pub code: i32,
    /// A string describing the error message
    pub message: String,
    /// Additional data specific to the error
    pub data: Option<json::Value>,
}

impl From<error::Error> for RpcError {
    fn from(e: error::Error) -> Self {
        RpcError {
            code: -1,
            message: format!("{e}"),
            data: None,
        }
    }
}

impl Into<json::Value> for RpcError {
    fn into(self) -> json::Value {
        json::to_value(self).unwrap()
    }
}

impl From<chan::RecvTimeoutError> for RpcError {
    fn from(value: chan::RecvTimeoutError) -> Self {
        RpcError {
            code: -1,
            message: value.to_string(),
            data: None,
        }
    }
}

impl Into<ErrorObject<'static>> for RpcError {
    fn into(self) -> ErrorObject<'static> {
        ErrorObject::owned(self.code, self.message, self.data)
    }
}

impl From<json::Error> for RpcError {
    fn from(value: json::Error) -> Self {
        RpcError {
            code: -1,
            message: format!("{value}"),
            data: None,
        }
    }
}

impl IntoResponse for RpcError {
    type Output = RpcError;

    fn into_response(self) -> ResponsePayload<'static, Self::Output> {
        ResponsePayload::Error(self.into())
    }
}
