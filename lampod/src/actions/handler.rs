//! Handler module implementation that
use std::sync::Arc;

use lightning::util::events::Event;

use crate::ln::{
    events::{ChangeStateChannelEvent, ChannelEvents, OpenChannelEvent},
    LampoChannelManager,
};

use super::Handler;

pub struct LampoHandler {
    channel_manager: Arc<LampoChannelManager>,
}

impl LampoHandler {
    pub fn new(channel: &Arc<LampoChannelManager>) -> Self {
        Self {
            channel_manager: channel.clone(),
        }
    }
}

impl Handler for LampoHandler {
    fn handle(&self, event: lightning::util::events::Event) -> anyhow::Result<()> {
        match event {
            Event::OpenChannelRequest {
                temporary_channel_id,
                counterparty_node_id,
                funding_satoshis,
                push_msat,
                channel_type,
            } => {
                unimplemented!()
            }
            Event::ChannelReady {
                channel_id,
                user_channel_id,
                counterparty_node_id,
                channel_type,
            } => {
                let event = ChangeStateChannelEvent {
                    channel_id,
                    node_id: counterparty_node_id,
                    channel_type,
                    state: crate::ln::events::ChannelState::Ready,
                };
                self.channel_manager.change_state_channel(event)
            }
            _ => unreachable!(),
        }
    }
}
