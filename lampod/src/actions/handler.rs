//! Handler module implementation that
use std::sync::Arc;

use lightning::events::Event;

use lampo_common::error;
use lampo_common::types::ChannelState;

use crate::events::LampoEvent;
use crate::ln::events::{ChangeStateChannelEvent, ChannelEvents, PeerEvents};
use crate::ln::{LampoChannelManager, LampoPeerManager};
use crate::LampoDeamon;

use super::{Handler, InventoryHandler};

pub struct LampoHandler {
    lampod: Arc<LampoDeamon>,
    channel_manager: Arc<LampoChannelManager>,
    peer_manager: Arc<LampoPeerManager>,
}

impl LampoHandler {
    pub fn new(lampod: Arc<LampoDeamon>) -> Self {
        Self {
            lampod: lampod.to_owned(),
            channel_manager: lampod.channel_manager(),
            peer_manager: lampod.peer_manager(),
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
            LampoEvent::InventoryEvent(event) => {
                self.lampod.handle(event)?;
                Ok(())
            }
        }
    }

    /// method used to handle the incoming event from ldk
    fn handle(&self, event: lightning::events::Event) -> error::Result<()> {
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
