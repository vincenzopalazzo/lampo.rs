//! Handler module implementation that
use std::sync::Arc;

use lightning::util::events::Event;

use lampo_common::error;
use lampo_common::types::ChannelState;

use crate::events::LampoEvent;
use crate::ln::events::{ChangeStateChannelEvent, ChannelEvents, PeerEvents};
use crate::ln::peer_manager::LampoPeerManager;
use crate::ln::LampoChannelManager;

use super::Handler;

pub struct LampoHandler {
    channel_manager: Arc<LampoChannelManager>,
    peer_manager: Arc<LampoPeerManager>,
}

impl LampoHandler {
    pub fn new(
        channel_manager: &Arc<LampoChannelManager>,
        peer_manager: &Arc<LampoPeerManager>,
    ) -> Self {
        Self {
            channel_manager: channel_manager.clone(),
            peer_manager: peer_manager.clone(),
        }
    }
}

#[allow(unused_variables)]
impl Handler for LampoHandler {
    async fn react(&self, event: crate::events::LampoEvent) -> error::Result<()> {
        match event {
            LampoEvent::LNEvent() => unimplemented!(),
            LampoEvent::OnChainEvent() => unimplemented!(),
            LampoEvent::PeerEvent(event) => self.peer_manager.handle(event).await,
            LampoEvent::InventoryEvent(_) => unimplemented!(),
        }
    }

    /// method used to handle the incoming event from ldk
    fn handle(&self, event: lightning::util::events::Event) -> error::Result<()> {
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
                    state: ChannelState::Ready,
                };
                self.channel_manager.change_state_channel(event)
            }
            _ => unreachable!(),
        }
    }
}
