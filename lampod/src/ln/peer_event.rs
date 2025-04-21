//! Implementation of all the peers events
use std::net::SocketAddr;

use tokio::sync::oneshot;

use lampo_common::{error, model::Connect, types::NodeId};

#[derive(Debug)]
pub enum PeerCommand {
    Connect(
        NodeId,
        SocketAddr,
        oneshot::Sender<Result<Connect, error::Error>>,
    ),
}
