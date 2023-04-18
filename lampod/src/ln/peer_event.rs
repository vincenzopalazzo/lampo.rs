//! Implementation of all the peers events
use std::net::SocketAddr;

use lampo_common::types::NodeId;

#[derive(Clone, PartialEq, Eq)]
pub enum PeerEvent {
    Connect(NodeId, SocketAddr),
}
