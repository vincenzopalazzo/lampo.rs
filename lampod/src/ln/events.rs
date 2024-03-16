//! Lightning Events handler implementation
use std::net::SocketAddr;

use async_trait::async_trait;

use lampo_common::error;
use lampo_common::ldk::ln::features::ChannelTypeFeatures;
use lampo_common::model::request;
use lampo_common::model::response;
use lampo_common::types::{ChannelId, ChannelState, NodeId};

use super::peer_event;

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
    fn open_channel(
        &self,
        open_channel: request::OpenChannel,
    ) -> error::Result<response::OpenChannel>;

    /// Close a channel
    fn close_channel(
        &self,
        close_channel: request::CloseChannel,
    ) -> error::Result<response::CloseChannel>;

    fn change_state_channel(&self, event: ChangeStateChannelEvent) -> error::Result<()>;
}

// FIXME: remove the async because we are using channels
#[async_trait]
pub trait PeerEvents {
    async fn handle(&self, event: peer_event::PeerCommand) -> error::Result<()>;

    async fn connect(&self, node_id: NodeId, host: SocketAddr) -> error::Result<()>;

    async fn disconnect(&self, node_id: NodeId) -> error::Result<()>;
}
