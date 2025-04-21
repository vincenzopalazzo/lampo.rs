//! All the Lampo Node Events that the node is able to react to
use lampo_common::chan;
use lampo_common::error;
use lampo_common::json;
use lampo_common::jsonrpc::Request;

/// All the event that are supported by the
/// Lampo Node.
///
/// This is the top level event enum, when it is possible
/// find the Lightning Node Events and the OnChainEvents.
#[derive(Debug, Clone)]
pub enum Command {
    /// External Event is done to be able to
    /// handle.
    ///
    /// An external handler can be any kind of method
    /// that lampod know nothing about.
    ///
    /// Core Lightning Plugins works this way and we want
    /// keep this freedom, but we do not want people
    /// that are couple with our design choice.
    ExternalCommand(Request<json::Value>),
}

impl Command {
    pub fn from_req(req: &Request<json::Value>) -> error::Result<Self> {
        match req.method.as_str() {
            _ => Ok(Command::ExternalCommand(req.clone())),
        }
    }
}
