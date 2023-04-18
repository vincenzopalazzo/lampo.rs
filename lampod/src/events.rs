//! All the Lampo Node Events that the node is able to react to

use crate::ln::peer_event::PeerEvent;

/// All the event that are supported by the
/// Lampo Node.
///
/// This is the top level event enum, when it is possible
/// find the Lightning Node Events and the OnChainEvents.
#[derive(PartialEq, Eq, Clone)]
pub enum LampoEvent {
    LNEvent(),
    OnChainEvent(),
    PeerEvent(PeerEvent),
    InventoryEvent(InventoryEvent),
}

#[derive(PartialEq, Eq, Clone)]
pub enum InventoryEvent {
    GetNodeInfo,
}
