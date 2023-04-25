use std::io;
use std::{error, fmt};

use serde::{Deserialize, Serialize};
use serde_json;

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
            Error::Rpc(rpc) => return rpc.clone(),
            _ => RpcError {
                code: -1,
                message: format!("{value}"),
                data: None,
            },
        }
    }
}
