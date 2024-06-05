//! All the Lampo Node Events that the node is able to react to
use crossbeam_channel as chan;

use lampo_async_jsonrpc::json_rpc2::Request;
use lampo_common::error;
use lampo_common::json;

use crate::ln::peer_event::PeerCommand;

/// All the event that are supported by the
/// Lampo Node.
///
/// This is the top level event enum, when it is possible
/// find the Lightning Node Events and the OnChainEvents.
#[derive(Debug, Clone)]
pub enum Command {
    LNCommand,
    OnChainCommand,
    PeerEvent(PeerCommand),
    InventoryEvent(InventoryCommand),
    /// External Event is done to be able to
    /// handle.
    ///
    /// An external handler can be any kind of method
    /// that lampod do not know nothing about.
    ///
    /// Core Lightning Plugins works this way and we want
    /// keep this freedom, but we do not want people
    /// that are couple with our design choice.
    ExternalCommand(Request<json::Value>, chan::Sender<json::Value>),
}

impl Command {
    pub fn from_req(
        req: &Request<json::Value>,
        chan: &chan::Sender<json::Value>,
    ) -> error::Result<Self> {
        match req.method.as_str() {
            "getinfo" => {
                let inner = InventoryCommand::from_req(req, chan)?;
                Ok(Self::InventoryEvent(inner))
            }
            _ => Ok(Command::ExternalCommand(req.clone(), chan.clone())),
        }
    }
}

#[derive(Debug, Clone)]
pub enum InventoryCommand {
    GetNodeInfo(chan::Sender<json::Value>),
}

impl InventoryCommand {
    pub fn from_req(
        req: &Request<json::Value>,
        chan: &chan::Sender<json::Value>,
    ) -> error::Result<Self> {
        match req.method.as_str() {
            "getinfo" => Ok(Self::GetNodeInfo(chan.clone())),
            _ => error::bail!("command {} not found", req.method),
        }
    }
}
