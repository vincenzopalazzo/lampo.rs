//! All the Lampo Node Events that the node is able to react to
use crossbeam_channel as chan;

use lampo_common::error;
use lampo_common::json;
use lampo_jsonrpc::json_rpc2::Request;

use crate::ln::peer_event::PeerEvent;

/// All the event that are supported by the
/// Lampo Node.
///
/// This is the top level event enum, when it is possible
/// find the Lightning Node Events and the OnChainEvents.
#[derive(Debug, Clone)]
pub enum LampoEvent {
    LNEvent(),
    OnChainEvent(),
    PeerEvent(PeerEvent),
    InventoryEvent(InventoryEvent),
}

impl LampoEvent {
    pub fn from_req(
        req: &Request<json::Value>,
        chan: &chan::Sender<json::Value>,
    ) -> error::Result<Self> {
        match req.method.as_str() {
            "getinfo" => {
                let inner = InventoryEvent::from_req(req, chan)?;
                Ok(Self::InventoryEvent(inner))
            }
            _ => error::bail!("command {} not found", req.method),
        }
    }
}

#[derive(Debug, Clone)]
pub enum InventoryEvent {
    GetNodeInfo(chan::Sender<json::Value>),
}

impl InventoryEvent {
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
