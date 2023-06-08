//! Handler module implementation that
use std::cell::RefCell;
use std::sync::Arc;

use lightning::events::Event;

use lampo_common::backend::Backend;
use lampo_common::error;
use lampo_common::types::ChannelState;

use crate::chain::{LampoChainManager, LampoWalletManager, WalletManager};
use crate::events::LampoEvent;
use crate::handler::external_handler::ExternalHandler;
use crate::ln::events::{ChangeStateChannelEvent, ChannelEvents, PeerEvents};
use crate::ln::{LampoChannelManager, LampoInventoryManager, LampoPeerManager};
use crate::utils::logger;
use crate::{async_run, LampoDeamon};

use super::{Handler, InventoryHandler};

pub struct LampoHandler {
    channel_manager: Arc<LampoChannelManager>,
    peer_manager: Arc<LampoPeerManager>,
    inventory_manager: Arc<LampoInventoryManager>,
    wallet_manager: Arc<dyn WalletManager>,
    chain_manager: Arc<LampoChainManager>,
    external_handlers: RefCell<Vec<Arc<dyn ExternalHandler>>>,
}

unsafe impl Send for LampoHandler {}
unsafe impl Sync for LampoHandler {}

impl LampoHandler {
    pub fn new(lampod: &LampoDeamon) -> Self {
        Self {
            channel_manager: lampod.channel_manager(),
            peer_manager: lampod.peer_manager(),
            inventory_manager: lampod.inventory_manager(),
            wallet_manager: lampod.wallet_manager(),
            chain_manager: lampod.onchain_manager(),
            external_handlers: RefCell::new(Vec::new()),
        }
    }

    pub fn add_external_handler(&self, handler: Arc<dyn ExternalHandler>) -> error::Result<()> {
        let mut vect = self.external_handlers.borrow_mut();
        vect.push(handler);
        Ok(())
    }
}

#[allow(unused_variables)]
impl Handler for LampoHandler {
    fn react(&self, event: crate::events::LampoEvent) -> error::Result<()> {
        match event {
            LampoEvent::LNEvent() => unimplemented!(),
            LampoEvent::OnChainEvent() => unimplemented!(),
            LampoEvent::PeerEvent(event) => {
                async_run!(self.peer_manager.handle(event))
            }
            LampoEvent::InventoryEvent(event) => {
                self.inventory_manager.handle(event)?;
                Ok(())
            }
            LampoEvent::ExternalEvent(req, chan) => {
                log::info!(
                    "external handler size {}",
                    self.external_handlers.borrow().len()
                );
                for handler in self.external_handlers.borrow().iter() {
                    if let Some(resp) = handler.handle(&req)? {
                        chan.send(resp)?;
                        return Ok(());
                    }
                }
                error::bail!("method `{}` not found", req.method);
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
            Event::ChannelClosed {
                channel_id,
                user_channel_id,
                reason,
            } => {
                log::info!("channel `{user_channel_id}` closed with reason: `{reason}`");
                Ok(())
            }
            Event::FundingGenerationReady {
                temporary_channel_id,
                counterparty_node_id,
                channel_value_satoshis,
                output_script,
                ..
            } => {
                log::info!("propagate funding transaction for open a channel with `{counterparty_node_id}`");
                let transaction = self
                    .wallet_manager
                    .create_transaction(output_script, channel_value_satoshis)?;
                self.chain_manager.backend.brodcast_tx(&transaction);
                log::info!("propagate transaction wit id `{}`", transaction.txid());
                Ok(())
            }
            _ => unreachable!(),
        }
    }
}
