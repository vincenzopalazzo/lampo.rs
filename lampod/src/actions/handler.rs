//! Handler module implementation that
use std::cell::RefCell;
use std::sync::Arc;

use lampo_common::chan;
use lampo_common::error;
use lampo_common::error::Ok;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::{Emitter, Event, Subscriber};
use lampo_common::handler::Handler as EventHandler;
use lampo_common::json;
use lampo_common::ldk;
use lampo_common::model::response::PaymentHop;
use lampo_common::model::response::PaymentState;
use lampo_common::types::ChannelState;

use crate::chain::{LampoChainManager, WalletManager};
use crate::command::Command;
use crate::handler::external_handler::ExternalHandler;
use crate::json_rpc2::Request;
use crate::ln::events::PeerEvents;
use crate::ln::{LampoChannelManager, LampoInventoryManager, LampoPeerManager};
use crate::{async_run, LampoDaemon};

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
    pub(crate) fn new(lampod: &LampoDaemon) -> Self {
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
    pub fn call<T: json::Serialize, R: json::DeserializeOwned>(
        &self,
        method: &str,
        args: T,
    ) -> error::Result<R> {
        let args = json::to_value(args)?;
        let request = Request::new(method, args);
        let (sender, receiver) = chan::bounded::<json::Value>(1);
        let command = Command::from_req(&request, &sender)?;
        log::info!("received {:?}", command);
        self.react(command)?;
        let result = receiver.recv()?;
        Ok(json::from_value::<R>(result)?)
    }
}

impl EventHandler for LampoHandler {
    fn emit(&self, event: Event) {
        log::debug!(target: "emitter", "emit event: {:?}", event);
        self.emitter.emit(event)
    }

    fn events(&self) -> chan::Receiver<Event> {
        log::debug!(target: "listener", "subscribe for events");
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
    fn handle(&self, event: ldk::events::Event) -> error::Result<()> {
        match event {
            ldk::events::Event::OpenChannelRequest {
                temporary_channel_id,
                counterparty_node_id,
                funding_satoshis,
                push_msat,
                channel_type,
            } => {
                Err(error::anyhow!("Request for open a channel received, unfortunatly we do not support this feature yet."))
            }
            ldk::events::Event::ChannelReady {
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
            },
            ldk::events::Event::ChannelClosed {
                channel_id,
                user_channel_id,
                reason,
                counterparty_node_id,
                channel_funding_txo,
                ..
            } => {
                if let Some(node_id) = counterparty_node_id {
                    log::warn!("closing channels with `{node_id}`");
                }
                let node_id = counterparty_node_id.map(|id| id.to_string());
                let txo = channel_funding_txo.map(|txo| txo.to_string());
                self.emit(Event::Lightning(LightningEvent::CloseChannelEvent { channel_id: channel_id.to_string(), message: reason.to_string(), counterparty_node_id : node_id, funding_utxo : txo}));
                log::info!("channel `{user_channel_id}` closed with reason: `{reason}`");
                Ok(())
            }
            ldk::events::Event::FundingGenerationReady {
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
                let fee = self.chain_manager.backend.fee_rate_estimation(6).map_err(|err| {
                    let msg = format!("Channel Opening Error: {err}");
                    self.emit(Event::Lightning(LightningEvent::ChannelEvent { state: ChannelState::OpeningError, message : msg}));
                    err
                })?;
                log::info!("fee estimated {:?} sats", fee);
                let transaction = self.wallet_manager.create_transaction(
                    output_script,
                    channel_value_satoshis,
                    fee,
                )?;
                log::info!("funding transaction created `{}`", transaction.txid());
                log::info!(
                    "transaction hex `{}`",
                    lampo_common::bitcoin::consensus::encode::serialize_hex(&transaction)
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
            ldk::events::Event::ChannelPending {
                counterparty_node_id,
                funding_txo,
                ..
            } => {
                log::info!(
                    "channel pending with node `{}` with funding `{funding_txo}`",
                    counterparty_node_id.to_string()
                );
                self.emit(Event::Lightning(LightningEvent::ChannelPending { counterparty_node_id, funding_transaction: funding_txo }));
                Ok(())
            }
            ldk::events::Event::PendingHTLCsForwardable { time_forwardable } => {
                self.channel_manager
                    .manager()
                    .process_pending_htlc_forwards();
                Ok(())
            }
            ldk::events::Event::PaymentClaimable {
                receiver_node_id,
                payment_hash,
                onion_fields,
                amount_msat,
                counterparty_skimmed_fee_msat,
                purpose,
                via_channel_id,
                via_user_channel_id,
                claim_deadline,
            } => {
                let preimage = match purpose {
                    ldk::events::PaymentPurpose::Bolt11InvoicePayment  {
                        payment_preimage, ..
                    } => payment_preimage,
                    ldk::events::PaymentPurpose::Bolt12OfferPayment { payment_preimage, .. } => payment_preimage,
                    ldk::events::PaymentPurpose::Bolt12RefundPayment { payment_preimage, .. } => payment_preimage,
                    ldk::events::PaymentPurpose::SpontaneousPayment(preimage) => Some(preimage),
                };
                self.channel_manager
                    .manager()
                    .claim_funds(preimage.unwrap());
                Ok(())
            }
            ldk::events::Event::PaymentClaimed {
                receiver_node_id,
                payment_hash,
                amount_msat,
                purpose,
                ..
            } => {
                let (payment_preimage, payment_secret) = match purpose {
                    ldk::events::PaymentPurpose::Bolt11InvoicePayment {
                        payment_preimage,
                        payment_secret,
                        ..
                    } => (payment_preimage, Some(payment_secret)),
                    ldk::events::PaymentPurpose::Bolt12OfferPayment { payment_preimage, payment_secret, .. } => (payment_preimage, Some(payment_secret)),
                    ldk::events::PaymentPurpose::Bolt12RefundPayment { payment_preimage, payment_secret, .. } => (payment_preimage, Some(payment_secret)),
                    ldk::events::PaymentPurpose::SpontaneousPayment(preimage) => (Some(preimage), None),
                };
                log::warn!("please note the payments are not make persistent for the moment");
                // FIXME: make peristant these information
                Ok(())
            }
            ldk::events::Event::PaymentSent { .. } => {
                log::info!("payment sent: `{:?}`", event);
                Ok(())
            },
            ldk::events::Event::PaymentPathSuccessful { payment_hash, path, .. } => {
                let path = path.hops.iter().map(|hop| PaymentHop::from(hop.clone())).collect::<Vec<PaymentHop>>();
                let hop = LightningEvent::PaymentEvent { state: PaymentState::Success, payment_hash: payment_hash.map(|hash| hash.to_string()), path };
                self.emit(Event::Lightning(hop));
                Ok(())
            },
            _ => Err(error::anyhow!("unexpected ldk event: {:?}", event)),
        }
    }
}
