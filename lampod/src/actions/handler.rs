//! Handler module implementation that
use std::cell::RefCell;
use std::sync::Arc;

use bitcoin::hashes::hex::ToHex;
use lampo_common::event::ln::LightningEvent;
use lampo_jsonrpc::json_rpc2::Request;
use lightning::events as ldk;

use lampo_common::chan;
use lampo_common::error;
use lampo_common::event::{Emitter, Event, Subscriber};
use lampo_common::handler::Handler as EventHandler;
use lampo_common::json;

use crate::chain::{LampoChainManager, WalletManager};
use crate::command::Command;
use crate::handler::external_handler::ExternalHandler;
use crate::ln::events::{ChangeStateChannelEvent, ChannelEvents, PeerEvents};
use crate::ln::{LampoChannelManager, LampoInventoryManager, LampoPeerManager};
use crate::{async_run, LampoDeamon};

use super::{Handler, InventoryHandler};

pub struct LampoHandler {
    channel_manager: Arc<LampoChannelManager>,
    peer_manager: Arc<LampoPeerManager>,
    inventory_manager: Arc<LampoInventoryManager>,
    wallet_manager: Arc<dyn WalletManager>,
    chain_manager: Arc<LampoChainManager>,
    external_handlers: RefCell<Vec<Arc<dyn ExternalHandler>>>,
    #[allow(dead_code)]
    emitter: Emitter<Event>,
    subscriber: Subscriber<Event>,
}

unsafe impl Send for LampoHandler {}
unsafe impl Sync for LampoHandler {}

impl LampoHandler {
    pub(crate) fn new(lampod: &LampoDeamon) -> Self {
        let emitter = Emitter::default();
        let subscriber = emitter.subscriber();
        Self {
            channel_manager: lampod.channel_manager(),
            peer_manager: lampod.peer_manager(),
            inventory_manager: lampod.inventory_manager(),
            wallet_manager: lampod.wallet_manager(),
            chain_manager: lampod.onchain_manager(),
            external_handlers: RefCell::new(Vec::new()),
            emitter,
            subscriber,
        }
    }

    pub fn add_external_handler(&self, handler: Arc<dyn ExternalHandler>) -> error::Result<()> {
        let mut vect = self.external_handlers.borrow_mut();
        vect.push(handler);
        Ok(())
    }

    /// Call any method supported by the lampod configuration. This includes
    /// a lot of handler code. This function serves as a broker pattern in some ways,
    /// but it may also function as a chain of responsibility pattern in certain cases.
    ///
    /// Welcome to the third design pattern in under 300 lines of code. The code will clarify the
    /// idea, but be prepared to see a broker pattern begin as a chain of responsibility pattern
    /// at some point.
    pub fn call<T: json::Serialize>(&self, method: &str, args: T) -> error::Result<json::Value> {
        let args = json::to_value(args)?;
        let request = Request::new(method, args);
        let (sender, receiver) = chan::bounded::<json::Value>(1);
        let command = Command::from_req(&request, &sender)?;
        log::info!("received {:?}", command);
        self.react(command)?;
        Ok(receiver.recv()?)
    }
}

impl EventHandler for LampoHandler {
    fn emit(&self, event: Event) {
        self.emitter.emit(event)
    }

    fn events(&self) -> chan::Receiver<Event> {
        self.subscriber.subscribe()
    }
}

#[allow(unused_variables)]
impl Handler for LampoHandler {
    fn react(&self, event: crate::command::Command) -> error::Result<()> {
        match event {
            Command::LNCommand => unimplemented!(),
            Command::OnChainCommand => unimplemented!(),
            Command::PeerEvent(event) => {
                async_run!(self.peer_manager.handle(event))
            }
            Command::InventoryEvent(event) => {
                self.inventory_manager.handle(event)?;
                Ok(())
            }
            Command::ExternalCommand(req, chan) => {
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
            ldk::Event::OpenChannelRequest {
                temporary_channel_id,
                counterparty_node_id,
                funding_satoshis,
                push_msat,
                channel_type,
            } => {
                unimplemented!()
            }
            ldk::Event::ChannelReady {
                channel_id,
                user_channel_id,
                counterparty_node_id,
                channel_type,
            } => {
                log::info!("channel ready with node `{counterparty_node_id}`, and channel type {channel_type}");
                self.emit(Event::Lightning(LightningEvent::ChannelReady {
                    counterparty_node_id,
                    channel_id,
                    channel_type,
                }));
                Ok(())
            }
            ldk::Event::ChannelClosed {
                channel_id,
                user_channel_id,
                reason,
            } => {
                log::info!("channel `{user_channel_id}` closed with reason: `{reason}`");
                Ok(())
            }
            ldk::Event::FundingGenerationReady {
                temporary_channel_id,
                counterparty_node_id,
                channel_value_satoshis,
                output_script,
                ..
            } => {
                self.emit(Event::Lightning(LightningEvent::FundingChannelStart {
                    counterparty_node_id,
                    temporary_channel_id,
                    channel_value_satoshis,
                }));

                log::info!("propagate funding transaction for open a channel with `{counterparty_node_id}`");
                // FIXME: estimate the fee rate with a callback
                let fee = self.chain_manager.backend.fee_rate_estimation(6);
                log::info!("fee estimated {fee} sats");
                let transaction = self.wallet_manager.create_transaction(
                    output_script,
                    channel_value_satoshis,
                    fee,
                )?;
                log::info!("funding transaction created `{}`", transaction.txid());
                log::info!(
                    "transaction hex `{}`",
                    lampo_common::bitcoin::consensus::serialize(&transaction).to_hex()
                );
                self.emit(Event::Lightning(LightningEvent::FundingChannelEnd {
                    counterparty_node_id,
                    temporary_channel_id,
                    channel_value_satoshis,
                    funding_transaction: transaction.clone(),
                }));
                self.channel_manager
                    .manager()
                    .funding_transaction_generated(
                        &temporary_channel_id,
                        &counterparty_node_id,
                        transaction,
                    )
                    .map_err(|err| error::anyhow!("{:?}", err))?;
                Ok(())
            }
            ldk::Event::ChannelPending {
                counterparty_node_id,
                funding_txo,
                ..
            } => {
                log::info!(
                    "channel pending with node `{}` with funding `{funding_txo}`",
                    counterparty_node_id.to_hex()
                );
                Ok(())
            }
            _ => unreachable!("{:?}", event),
        }
    }
}
