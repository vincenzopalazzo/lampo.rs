//! Implementation of all the peers events
use std::net::SocketAddr;

use crossbeam_channel as chan;

use crate::common::Connect;
use crate::common::NodeId;

#[derive(Debug, Clone)]
pub enum PeerCommand {
    Connect(NodeId, SocketAddr, chan::Sender<Connect>),
}
