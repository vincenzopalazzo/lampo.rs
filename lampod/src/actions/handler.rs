//! Handler module implementation that
use std::sync::Arc;

use lampo_common::bitcoin::absolute::Height;
use tokio::sync::RwLock;

use lampo_common::async_trait;
use lampo_common::bitcoin::Amount;
use lampo_common::bitcoin::FeeRate;
use lampo_common::chan;
use lampo_common::error;
use lampo_common::error::Ok;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::{Emitter, Event, Subscriber};
use lampo_common::handler::ExternalHandler;
use lampo_common::handler::Handler as EventHandler;
use lampo_common::json;
use lampo_common::jsonrpc::Request;
use lampo_common::ldk;
use lampo_common::model::response::PaymentHop;
use lampo_common::model::response::PaymentState;

use crate::chain::{LampoChainManager, WalletManager};
use crate::command::Command;
use crate::ln::{LampoChannelManager, LampoInventoryManager, LampoPeerManager};
use crate::LampoDaemon;

use super::Handler;

pub struct LampoHandler {
    channel_manager: Arc<LampoChannelManager>,
    peer_manager: Arc<LampoPeerManager>,
    inventory_manager: Arc<LampoInventoryManager>,
    wallet_manager: Arc<dyn WalletManager>,
    chain_manager: Arc<LampoChainManager>,
    external_handlers: RwLock<Vec<Arc<dyn ExternalHandler>>>,
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
            external_handlers: RwLock::new(Vec::new()),
            emitter,
            subscriber,
        }
    }

    pub async fn add_external_handler(
        &self,
        handler: Arc<dyn ExternalHandler>,
    ) -> error::Result<()> {
        let mut external_handlers = self.external_handlers.write().await;
        external_handlers.push(handler);
        Ok(())
    }

    /// Call any method supported by the lampod configuration. This includes
    /// a lot of handler code. This function serves as a broker pattern in some ways,
    /// but it may also function as a chain of responsibility pattern in certain cases.
    pub async fn call<T: json::Serialize, R: json::DeserializeOwned>(
        &self,
        method: &str,
        args: T,
    ) -> error::Result<R> {
        let args = json::to_value(args)?;
        let request = Request::new(method, args);
        let command = Command::from_req(&request)?;
        log::info!("received {:?}", command);
        let result = self.react(command).await?;
        Ok(json::from_value::<R>(result)?)
    }
}

impl EventHandler for LampoHandler {
    fn emit(&self, event: Event) {
        log::debug!(target: "emitter", "emit event: {:?}", event);
        self.emitter.emit(event)
    }

    fn events(&self) -> chan::UnboundedReceiver<Event> {
        log::debug!(target: "listener", "subscribe for events");
        self.subscriber.subscribe()
    }
}

#[async_trait]
impl Handler for LampoHandler {
    // FIXME: this is not needed anymore? we can assume that all command are external?
    async fn react(&self, event: crate::command::Command) -> error::Result<json::Value> {
        let handler = self.external_handlers.read().await;
        match event {
            Command::ExternalCommand(req) => {
                log::debug!(target: "lampo", "external handler size {}", handler.len());
                for handler in handler.iter() {
                    if let Some(resp) = handler.handle(&req).await? {
                        return Ok(resp);
                    }
                }
                error::bail!("method `{}` not found", req.method);
            }
        }
    }

    /// method used to handle the incoming event from ldk
    async fn handle(&self, event: ldk::events::Event) -> error::Result<()> {
        log::debug!(target: "lampo", "handle ldk event: {:?}", event);
        self.emit(Event::RawLDK(event.clone()));
        match event {
            ldk::events::Event::OpenChannelRequest {
                temporary_channel_id,
                counterparty_node_id,
                funding_satoshis,
                channel_type,
                channel_negotiation_type: _,
                is_announced: _,
                params: _
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

                // Provide detailed closure reason based on the ClosureReason enum
                let detailed_reason = match reason {
                    ldk::events::ClosureReason::CounterpartyForceClosed { peer_msg } => {
                        format!("Counterparty force-closed the channel. Peer message: {}", peer_msg)
                    },
                    ldk::events::ClosureReason::HolderForceClosed { broadcasted_latest_txn } => {
                        let broadcast_status = match broadcasted_latest_txn {
                            Some(true) => "with broadcasting latest transaction",
                            Some(false) => "without broadcasting latest transaction",
                            None => "broadcast status unknown"
                        };
                        format!("We force-closed the channel {}", broadcast_status)
                    },
                    ldk::events::ClosureReason::LegacyCooperativeClosure => {
                        "Channel closed cooperatively (legacy closure)".to_string()
                    },
                    ldk::events::ClosureReason::CounterpartyInitiatedCooperativeClosure => {
                        "Counterparty initiated cooperative channel closure".to_string()
                    },
                    ldk::events::ClosureReason::LocallyInitiatedCooperativeClosure => {
                        "We initiated cooperative channel closure".to_string()
                    },
                    ldk::events::ClosureReason::CommitmentTxConfirmed => {
                        "Channel closed due to commitment transaction confirmation on-chain".to_string()
                    },
                    ldk::events::ClosureReason::FundingTimedOut => {
                        "Channel funding transaction failed to confirm in time".to_string()
                    },
                    ldk::events::ClosureReason::ProcessingError { err } => {
                        format!("Channel closed due to processing error: {}", err)
                    },
                    ldk::events::ClosureReason::DisconnectedPeer => {
                        "Peer disconnected before funding completed, channel forgotten".to_string()
                    },
                    ldk::events::ClosureReason::OutdatedChannelManager => {
                        "Channel closed due to outdated ChannelManager (ChannelMonitor is newer)".to_string()
                    },
                    ldk::events::ClosureReason::CounterpartyCoopClosedUnfundedChannel => {
                        "Counterparty requested cooperative close of unfunded channel".to_string()
                    },
                    ldk::events::ClosureReason::FundingBatchClosure => {
                        "Channel closed because another channel in the same funding batch closed".to_string()
                    },
                    ldk::events::ClosureReason::HTLCsTimedOut => {
                        "Channel closed due to HTLC timeout".to_string()
                    },
                    ldk::events::ClosureReason::PeerFeerateTooLow { peer_feerate_sat_per_kw, required_feerate_sat_per_kw } => {
                        format!("Channel closed due to peer's feerate too low. Peer feerate: {} sat/kw, Required: {} sat/kw",
                               peer_feerate_sat_per_kw, required_feerate_sat_per_kw)
                    },
                };

                let node_id = counterparty_node_id.map(|id| id.to_string());
                let txo = channel_funding_txo.map(|txo| txo.to_string());
                self.emit(Event::Lightning(LightningEvent::CloseChannelEvent {
                    channel_id: channel_id.to_string(),
                    message: detailed_reason.clone(),
                    counterparty_node_id: node_id,
                    funding_utxo: txo
                }));
                log::info!("channel `{user_channel_id}` closed: {}", detailed_reason);
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
                let fee = self.chain_manager.backend.fee_rate_estimation(6).await.map_err(|err| {
                    let msg = format!("Channel Opening Error: {err}");
                    self.emit(Event::Lightning(LightningEvent::ChannelEvent { state: "error".to_owned(), message : msg}));
                    err
                })?;
                log::info!("fee estimated {:?} sats", fee);

                let best_block = self.channel_manager.manager().current_best_block().height;
                let transaction = self.wallet_manager.create_transaction(
                    output_script,
                    Amount::from_sat(channel_value_satoshis),
                    FeeRate::from_sat_per_vb_unchecked(fee as u64),
                    // FIXME: remove unwrap
                    Height::from_consensus(best_block).unwrap(),
                ).await?;
                log::info!("funding transaction created `{}`", transaction.compute_txid());
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
                        temporary_channel_id,
                        counterparty_node_id,
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
                payment_id: _,
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
                // FIXME: make peristent these information
                Ok(())
            }
            ldk::events::Event::PaymentSent { .. } => {
                log::info!("payment sent: `{:?}`", event);
                Ok(())
            },
            ldk::events::Event::PaymentPathSuccessful { payment_hash, path, .. } => {
                let path = path.hops.iter().map(|hop| PaymentHop::from(hop.clone())).collect::<Vec<PaymentHop>>();
                let hop = LightningEvent::PaymentEvent { state: PaymentState::Success, payment_hash: payment_hash.map(|hash| hash.to_string()), path, reason: None };
                self.emit(Event::Lightning(hop));
                Ok(())
            },
            ldk::events::Event::PaymentFailed { payment_id, payment_hash, reason } => {
                log::error!("payment failed: {:?} with reason: {:?}", payment_id, reason);

                // Provide detailed failure reason based on PaymentFailureReason enum
                let detailed_reason = match reason {
                    Some(ldk::events::PaymentFailureReason::RecipientRejected) => {
                        "Payment was rejected by the recipient. The destination node refused to accept the payment.".to_string()
                    },
                    Some(ldk::events::PaymentFailureReason::UserAbandoned) => {
                        "Payment was abandoned by the user before completion.".to_string()
                    },
                    Some(ldk::events::PaymentFailureReason::RetriesExhausted) => {
                        "Payment failed after exhausting all retry attempts. No more routes available to try.".to_string()
                    },
                    Some(ldk::events::PaymentFailureReason::PaymentExpired) => {
                        "Payment expired before it could be completed. The invoice or payment request has timed out.".to_string()
                    },
                    Some(ldk::events::PaymentFailureReason::RouteNotFound) => {
                        "No route found to the destination. This could be due to insufficient liquidity, \
                         network connectivity issues, or the destination being unreachable.".to_string()
                    },
                    Some(ldk::events::PaymentFailureReason::UnexpectedError) => {
                        "Payment failed due to an unexpected error. Please check logs for more details.".to_string()
                    },
                    Some(ldk::events::PaymentFailureReason::UnknownRequiredFeatures) => {
                        "Payment failed due to unknown required features. The destination requires features \
                         that are not supported by this node.".to_string()
                    },
                    Some(ldk::events::PaymentFailureReason::InvoiceRequestExpired) => {
                        "The invoice request has expired before the payment could be completed.".to_string()
                    },
                    Some(ldk::events::PaymentFailureReason::InvoiceRequestRejected) => {
                        "The invoice request was rejected by the recipient.".to_string()
                    },
                    Some(ldk::events::PaymentFailureReason::BlindedPathCreationFailed) => {
                        "Failed to create a blinded path for the payment. This may indicate routing issues.".to_string()
                    },
                    None => {
                        "Payment failed for an unknown reason.".to_string()
                    },
                };

                let hop = LightningEvent::PaymentEvent {
                    state: PaymentState::Failure,
                    payment_hash: payment_hash.map(|hash| hash.to_string()),
                    path: vec![],
                    reason: Some(detailed_reason)
                };
                self.emit(Event::Lightning(hop));
                Ok(())
            },
            _ => {
                log::warn!(target: "lampo::handler", "unhandled ldk event: {:?}", event);
                Ok(())
            },
        }
    }
}
