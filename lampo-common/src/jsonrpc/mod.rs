use std::{error, fmt, io};

use serde::{Deserialize, Serialize};
use serde_json;

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
    pub id: Id,
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

/// A library error
#[derive(Debug)]
pub enum Error {
    /// Json error
    Json(serde_json::Error),
    /// IO Error
    Io(io::Error),
    /// Error response
    Rpc(RpcError),
    /// Response has neither error nor result
    NoErrorOrResult,
    /// Response to a request did not have the expected nonce
    NonceMismatch,
    /// Response to a request had a jsonrpc field other than "2.0"
    VersionMismatch,
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Error {
        Error::Json(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

impl From<RpcError> for Error {
    fn from(e: RpcError) -> Error {
        Error::Rpc(e)
    }
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Error {
        Error::Rpc(RpcError {
            code: -1,
            message: format!("{e}"),
            data: None,
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::Json(ref e) => write!(f, "JSON decode error: {e}"),
            Error::Io(ref e) => write!(f, "IO error response: {e}"),
            Error::Rpc(ref r) => write!(f, "RPC error response: {r:?}"),
            Error::NoErrorOrResult => write!(f, "Malformed RPC response"),
            Error::NonceMismatch => write!(f, "Nonce of response did not match nonce of request"),
            Error::VersionMismatch => write!(f, "`jsonrpc` field set to non-\"2.0\""),
        }
    }
}

impl error::Error for Error {
    fn cause(&self) -> Option<&dyn error::Error> {
        match *self {
            Error::Json(ref e) => Some(e),
            _ => None,
        }
    }
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
    pub data: Option<serde_json::Value>,
}

impl From<Error> for RpcError {
    fn from(value: Error) -> Self {
        match value {
            Error::Rpc(rpc) => rpc.clone(),
            _ => RpcError {
                code: -1,
                message: format!("{value}"),
                data: None,
            },
        }
    }
}
