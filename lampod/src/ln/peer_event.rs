//! Implementation of all the peers events
use std::net::SocketAddr;

use lampo_common::chan;
use lampo_common::model::Connect;
use lampo_common::types::NodeId;

#[derive(Debug, Clone)]
pub enum PeerCommand {
    Connect(NodeId, SocketAddr, chan::Sender<Connect>),
}
