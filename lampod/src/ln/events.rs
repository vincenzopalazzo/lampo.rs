//! Lightning Events handler implementation
use std::{future::Future, net::SocketAddr};

use bitcoin::secp256k1::PublicKey;
use lightning::{
    ln::{features::ChannelTypeFeatures, msgs::NetAddress},
    util::{config::UserConfig, events::Event},
};

pub type NodeId = PublicKey;
pub type ChannelId = [u8; 32];

pub enum ChannelState {
    Opening,
    Ready,
}

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
    fn open_channel(&self, open_channel: OpenChannelEvent) -> anyhow::Result<()>;

    /// Close a channel
    fn close_channel(&self) -> anyhow::Result<()>;

    fn change_state_channel(&self, event: ChangeStateChannelEvent) -> anyhow::Result<()>;
}

pub trait PeerEvents {
    async fn connect(&self, node_id: NodeId, host: SocketAddr) -> anyhow::Result<()>;

    async fn disconnect(&self, node_id: NodeId) -> anyhow::Result<()>;
}
