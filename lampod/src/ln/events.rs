//! Lightning Events handler implementation
use std::net::SocketAddr;

use lightning::{ln::features::ChannelTypeFeatures, util::config::UserConfig};

use lampo_common::error;
use lampo_common::types::{ChannelId, ChannelState, NodeId};

use super::peer_event;

pub struct OpenChannelEvent {
    pub node_id: NodeId,
    pub amount: u64,
    pub push_msat: u64,
    pub channel_id: ChannelId,
    pub config: Option<UserConfig>,
}

pub struct OpenChannelResult {
    pub tmp_channel_id: String,
}

pub struct ChangeStateChannelEvent {
    pub channel_id: ChannelId,
    pub node_id: NodeId,
    pub channel_type: ChannelTypeFeatures,
    pub state: ChannelState,
}

/// Lightning Network Channels events
pub trait ChannelEvents {
    /// Open a Channel
    fn open_channel(&self, open_channel: OpenChannelEvent) -> error::Result<()>;

    /// Close a channel
    fn close_channel(&self) -> error::Result<()>;

    fn change_state_channel(&self, event: ChangeStateChannelEvent) -> error::Result<()>;
}

// FIXME: remove the async because we are using channels
pub trait PeerEvents {
    async fn handle(&self, event: peer_event::PeerEvent) -> error::Result<()>;

    async fn connect(&self, node_id: NodeId, host: SocketAddr) -> error::Result<()>;

    async fn disconnect(&self, node_id: NodeId) -> error::Result<()>;
}
