//! Implementation of all the peers events
use std::net::SocketAddr;

use crossbeam_channel as chan;

use lampo_common::{model::Connect, types::NodeId};

#[derive(Debug, Clone)]
pub enum PeerEvent {
    Connect(NodeId, SocketAddr, chan::Sender<Connect>),
}
