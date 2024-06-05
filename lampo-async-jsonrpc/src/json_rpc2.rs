use serde::{Deserialize, Serialize};

pub use crate::errors::{Error, RpcError};

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

#[allow(clippy::derive_partial_eq_without_eq)]
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

#[allow(clippy::derive_partial_eq_without_eq)]
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

impl<T> Response<T> {
    /// Extract the result from a response, consuming the response
    pub fn into_result(self) -> Result<T, Error> {
        if let Some(e) = self.error {
            return Err(Error::Rpc(e));
        }

        self.result.ok_or(Error::NoErrorOrResult)
    }

    /// Returns whether or not the `result` field is empty
    pub fn is_none(&self) -> bool {
        self.result.is_none()
    }
}
