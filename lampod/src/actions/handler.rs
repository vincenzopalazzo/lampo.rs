//! Handler module implementation that
use std::net::SocketAddr;
use std::sync::Arc;

use lampo_common::bitcoin::absolute::Height;
use tokio::sync::RwLock;

use lampo_common::async_trait;
use lampo_common::bitcoin::Amount;
use lampo_common::chan;
use lampo_common::error;
use lampo_common::error::Ok;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::onchain::OnChainEvent;
use lampo_common::event::{Emitter, Event, Subscriber};
use lampo_common::handler::ExternalHandler;
use lampo_common::handler::Handler as EventHandler;
use lampo_common::json;
use lampo_common::jsonrpc::Request;
use lampo_common::ldk;
use lampo_common::ldk::chain::chaininterface::{BroadcasterInterface, TransactionType};
use lampo_common::ldk::sign::NodeSigner;
use lampo_common::model::response::PaymentHop;
use lampo_common::model::response::PaymentState;

use crate::chain::{FeeTarget, LampoChainManager, WalletManager};
use crate::command::Command;
use crate::ln::payer_proof::{self, PayerProofRecord};
use crate::ln::{LampoChannelManager, LampoInventoryManager, LampoPeerManager};
use crate::persistence::LampoPersistence;
use crate::LampoDaemon;

use super::Handler;

pub struct LampoHandler {
    channel_manager: Arc<LampoChannelManager>,
    peer_manager: Arc<LampoPeerManager>,
    inventory_manager: Arc<LampoInventoryManager>,
    wallet_manager: Arc<dyn WalletManager>,
    chain_manager: Arc<LampoChainManager>,
    persister: Arc<LampoPersistence>,
    external_handlers: RwLock<Vec<Arc<dyn ExternalHandler>>>,
    #[allow(dead_code)]
    emitter: Emitter<Event>,
    subscriber: Subscriber<Event>,
}

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
            persister: lampod.persister(),
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

    pub fn peer_manager(&self) -> Arc<LampoPeerManager> {
        self.peer_manager.clone()
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
        // Never hold the read guard across the `handle().await`: a parked
        // external handler (e.g. a replayed command after an unclean
        // shutdown whose reply path never completes) would keep the guard
        // forever and starve `add_external_handler`'s write() — observed
        // live as a full startup hang (issue #576: log stops before
        // `Starting Server`, API never binds, pid lock held). Cloning the
        // Arc list keeps the critical section await-free.
        let external_handlers = self.external_handlers.read().await.clone();
        match event {
            Command::ExternalCommand(req) => {
                log::debug!(target: "lampo", "external handler size {}", external_handlers.len());
                for handler in external_handlers.iter() {
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
                ..
            } => {
                // LDK 0.3 removed `manually_accept_inbound_channels`; inbound
                // channels are now always surfaced here and must be accepted
                // explicitly. Auto-accept to preserve the previous behaviour.
                log::info!(
                    target: "lampod",
                    "accepting inbound channel request from `{counterparty_node_id}`"
                );
                self.channel_manager
                    .manager()
                    .accept_inbound_channel(&temporary_channel_id, &counterparty_node_id, 0, None)
                    .map_err(|err| error::anyhow!("{:?}", err))?;
                Ok(())
            }
            ldk::events::Event::ChannelReady {
                channel_id,
                user_channel_id,
                counterparty_node_id,
                channel_type,
                ..
            } => {
                log::info!("channel ready with node `{counterparty_node_id}`, and channel type {channel_type}");
                self.emit(Event::Lightning(LightningEvent::ChannelReady {
                    counterparty_node_id,
                    channel_id,
                    channel_type,
                }));
                Ok(())
            }
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
                        format!(
                            "Counterparty force-closed the channel. Peer message: {}",
                            peer_msg
                        )
                    }
                    ldk::events::ClosureReason::HolderForceClosed {
                        broadcasted_latest_txn,
                        ..
                    } => {
                        let broadcast_status = match broadcasted_latest_txn {
                            Some(true) => "with broadcasting latest transaction",
                            Some(false) => "without broadcasting latest transaction",
                            None => "broadcast status unknown",
                        };
                        format!("We force-closed the channel {}", broadcast_status)
                    }
                    ldk::events::ClosureReason::LegacyCooperativeClosure => {
                        "Channel closed cooperatively (legacy closure)".to_string()
                    }
                    ldk::events::ClosureReason::CounterpartyInitiatedCooperativeClosure => {
                        "Counterparty initiated cooperative channel closure".to_string()
                    }
                    ldk::events::ClosureReason::LocallyInitiatedCooperativeClosure => {
                        "We initiated cooperative channel closure".to_string()
                    }
                    ldk::events::ClosureReason::CommitmentTxConfirmed => {
                        "Channel closed due to commitment transaction confirmation on-chain"
                            .to_string()
                    }
                    ldk::events::ClosureReason::FundingTimedOut => {
                        "Channel funding transaction failed to confirm in time".to_string()
                    }
                    ldk::events::ClosureReason::ProcessingError { err } => {
                        format!("Channel closed due to processing error: {}", err)
                    }
                    ldk::events::ClosureReason::DisconnectedPeer => {
                        "Peer disconnected before funding completed, channel forgotten".to_string()
                    }
                    ldk::events::ClosureReason::OutdatedChannelManager => {
                        "Channel closed due to outdated ChannelManager (ChannelMonitor is newer)"
                            .to_string()
                    }
                    ldk::events::ClosureReason::CounterpartyCoopClosedUnfundedChannel => {
                        "Counterparty requested cooperative close of unfunded channel".to_string()
                    }
                    ldk::events::ClosureReason::LocallyCoopClosedUnfundedChannel => {
                        "We requested cooperative close of an unfunded channel".to_string()
                    }
                    ldk::events::ClosureReason::FundingBatchClosure => {
                        "Channel closed because another channel in the same funding batch closed"
                            .to_string()
                    }
                    ldk::events::ClosureReason::HTLCsTimedOut { .. } => {
                        "Channel closed due to HTLC timeout".to_string()
                    }
                    ldk::events::ClosureReason::PeerFeerateTooLow {
                        peer_feerate_sat_per_kw,
                        required_feerate_sat_per_kw,
                    } => {
                        format!("Channel closed due to peer's feerate too low. Peer feerate: {} sat/kw, Required: {} sat/kw",
                               peer_feerate_sat_per_kw, required_feerate_sat_per_kw)
                    }
                };

                let node_id = counterparty_node_id.map(|id| id.to_string());
                let txo = channel_funding_txo.map(|txo| txo.to_string());
                self.emit(Event::Lightning(LightningEvent::CloseChannelEvent {
                    channel_id: channel_id.to_string(),
                    message: detailed_reason.clone(),
                    counterparty_node_id: node_id,
                    funding_utxo: txo,
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
                // Drop the outbound channel when funding cannot proceed: LDK
                // still holds the temporary channel after `FundingGenerationReady`,
                // and leaving it live after a fee/wallet failure means a retry
                // races the original open while the waiter already returned.
                let abandon_temp_channel = |this: &LampoHandler, msg: &str| {
                    this.emit(Event::OnChain(OnChainEvent::FundingChannelFailed {
                        temporary_channel_id: Some(temporary_channel_id.to_string()),
                        txid: None,
                        reason: msg.to_owned(),
                    }));
                    if let Err(err) = this
                        .channel_manager
                        .manager()
                        .force_close_broadcasting_latest_txn(
                            &temporary_channel_id,
                            &counterparty_node_id,
                            msg.to_owned(),
                        )
                    {
                        log::warn!(
                            target: "lampo",
                            "failed to abandon channel `{temporary_channel_id}` after funding error: {err:?}"
                        );
                    }
                };
                // Same as ldk-node: ChannelFunding is 12-block economical,
                // read from the cache (never block the event loop on RPC).
                let fee_rate = self
                    .chain_manager
                    .estimate_fee_rate(FeeTarget::ChannelFunding);
                log::info!(
                    target: "lampo",
                    "funding fee rate {} sat/kW",
                    fee_rate.to_sat_per_kwu()
                );

                let best_block = self.channel_manager.manager().current_best_block().height;
                let transaction = match self
                    .wallet_manager
                    .create_transaction(
                        output_script,
                        Amount::from_sat(channel_value_satoshis),
                        fee_rate,
                        // FIXME: remove unwrap
                        Height::from_consensus(best_block).unwrap(),
                    )
                    .await
                {
                    std::result::Result::Ok(tx) => tx,
                    Err(err) => {
                        let msg = format!("Failed to create funding transaction: {err}");
                        log::error!(target: "lampo", "{}", msg);
                        abandon_temp_channel(self, &msg);
                        return Err(err);
                    }
                };
                log::info!(
                    "funding transaction created `{}`",
                    transaction.compute_txid()
                );
                log::info!(
                    "transaction hex `{}`",
                    lampo_common::bitcoin::consensus::encode::serialize_hex(&transaction)
                );
                // Hand the tx to LDK *before* emitting `FundingChannelEnd`,
                // under the shared wait-state lock so a concurrent timeout
                // cannot force-close an already-accepted funding (and so we
                // skip handoff if the waiter already abandoned).
                match self
                    .channel_manager
                    .funding_transaction_generated_if_waiting(
                        temporary_channel_id,
                        counterparty_node_id,
                        transaction.clone(),
                    ) {
                    std::result::Result::Ok(false) => {
                        let msg =
                            "funding wait already abandoned; skipped funding_transaction_generated"
                                .to_owned();
                        log::warn!(target: "lampo", "{}", msg);
                        abandon_temp_channel(self, &msg);
                        return Err(error::anyhow!("{}", msg));
                    }
                    std::result::Result::Err(err) => {
                        let msg = format!("funding_transaction_generated failed: {err}");
                        log::error!(target: "lampo", "{}", msg);
                        abandon_temp_channel(self, &msg);
                        return Err(err);
                    }
                    std::result::Result::Ok(true) => {}
                }
                self.emit(Event::Lightning(LightningEvent::FundingChannelEnd {
                    counterparty_node_id,
                    temporary_channel_id,
                    channel_value_satoshis,
                    funding_transaction: transaction,
                }));
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
                self.emit(Event::Lightning(LightningEvent::ChannelPending {
                    counterparty_node_id,
                    funding_transaction: funding_txo,
                }));
                Ok(())
            }
            ldk::events::Event::PaymentClaimable {
                receiver_node_id,
                payment_hash,
                onion_fields,
                amount_msat,
                counterparty_skimmed_fee_msat,
                purpose,
                claim_deadline,
                payment_id: _,
                ..
            } => {
                let preimage = match purpose {
                    ldk::events::PaymentPurpose::Bolt11InvoicePayment {
                        payment_preimage, ..
                    } => payment_preimage,
                    ldk::events::PaymentPurpose::Bolt12OfferPayment {
                        payment_preimage, ..
                    } => payment_preimage,
                    ldk::events::PaymentPurpose::Bolt12RefundPayment {
                        payment_preimage, ..
                    } => payment_preimage,
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
                    ldk::events::PaymentPurpose::Bolt12OfferPayment {
                        payment_preimage,
                        payment_secret,
                        ..
                    } => (payment_preimage, Some(payment_secret)),
                    ldk::events::PaymentPurpose::Bolt12RefundPayment {
                        payment_preimage,
                        payment_secret,
                        ..
                    } => (payment_preimage, Some(payment_secret)),
                    ldk::events::PaymentPurpose::SpontaneousPayment(preimage) => {
                        (Some(preimage), None)
                    }
                };
                log::warn!("please note the payments are not make persistent for the moment");
                // FIXME: make peristent these information
                Ok(())
            }
            ldk::events::Event::SpendableOutputs {
                outputs,
                channel_id,
                counterparty_node_id,
            } => {
                log::info!(
                    target: "lampo::handler",
                    "tracking {} spendable output(s) from channel `{channel_id:?}` for sweeping",
                    outputs.len(),
                );
                // `exclude_static_outputs` must stay `false`: static outputs
                // pay to scripts derived from the LDK keys manager, which the
                // BDK on-chain wallet does not track. Only the sweeper can
                // claim them.
                self.channel_manager
                    .sweeper()
                    .track_spendable_outputs(outputs, channel_id, counterparty_node_id, false, None)
                    .await
                    .map_err(|_| {
                        error::anyhow!("failed to persist spendable outputs in the sweeper")
                    })?;
                Ok(())
            }
            ldk::events::Event::PaymentSent {
                payment_id,
                payment_preimage,
                payment_hash,
                bolt12_invoice,
                ..
            } => {
                log::info!(
                    target: "lampo::handler",
                    "payment sent: id `{payment_id:?}`, bolt12 payer proof available: {}",
                    bolt12_invoice.is_some()
                );
                let Some(payment_id) = payment_id else {
                    // Without an id the receipt cannot be matched to the `pay`
                    // call that is waiting for it, so there is nothing to emit.
                    return Ok(());
                };
                let record = PayerProofRecord {
                    preimage: payment_preimage,
                    invoice: bolt12_invoice,
                };

                // Build the proof from the material in hand rather than from
                // storage, so the receipt does not depend on the write below.
                let expanded_key = self
                    .wallet_manager
                    .ldk_keys()
                    .keys_manager
                    .get_expanded_key();
                let payer_proof = payer_proof::build(&record, &expanded_key, payment_id);

                self.emit(Event::Lightning(LightningEvent::PaymentReceipt {
                    payment_id: lampo_common::hex::encode(payment_id.0),
                    payment_preimage: lampo_common::hex::encode(record.preimage.0),
                    payer_proof,
                }));

                // This is the only event carrying the paid BOLT 12 invoice, and the
                // proof cannot be rebuilt without it. Keep it so the proof can be
                // re-issued later with a wider disclosure. The payment already
                // settled and the receipt is already out, so a storage failure
                // only costs us the ability to re-issue.
                //
                // Only BOLT 12 payments are worth keeping: a BOLT 11 record holds
                // a preimage and nothing else, can never build a proof, and
                // nothing prunes it.
                if record.invoice.is_some() {
                    if let Err(err) = payer_proof::store(&self.persister, &payment_hash, &record) {
                        log::error!(target: "lampo::handler", "storing payer proof material: {err}");
                    }
                }
                Ok(())
            }
            ldk::events::Event::PaymentPathSuccessful {
                payment_id,
                payment_hash,
                path,
                ..
            } => {
                let path = path
                    .hops
                    .iter()
                    .map(|hop| PaymentHop::from(hop.clone()))
                    .collect::<Vec<PaymentHop>>();
                let hop = LightningEvent::PaymentEvent {
                    state: PaymentState::Success,
                    payment_id: Some(lampo_common::hex::encode(payment_id.0)),
                    payment_hash: payment_hash.map(|hash| hash.to_string()),
                    path,
                    reason: None,
                };
                self.emit(Event::Lightning(hop));
                Ok(())
            }
            ldk::events::Event::PaymentFailed {
                payment_id,
                payment_hash,
                reason,
            } => {
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
                    payment_id: Some(lampo_common::hex::encode(payment_id.0)),
                    payment_hash: payment_hash.map(|hash| hash.to_string()),
                    path: vec![],
                    reason: Some(detailed_reason),
                };
                self.emit(Event::Lightning(hop));
                Ok(())
            }
            ldk::events::Event::ConnectionNeeded { node_id, addresses } => {
                // LDK cannot deliver an onion message to a node it has no
                // connection to, and asks us to open one. Dropping this is
                // why BOLT12 offers failed with `InvoiceRequestExpired`
                // whenever the blinded path's introduction node was not
                // already a peer: the invoice request was never sent at all.
                //
                // Connecting blocks until the peer disconnects, so it runs on
                // its own task -- the event handler must not stall the whole
                // LDK event loop on a single dial. Addresses are tried in
                // order, stopping at the first that connects, as ldk-node does.
                let peer_manager = self.peer_manager.clone();
                tokio::spawn(async move {
                    for address in addresses {
                        // Only the plain TCP forms can be dialled directly.
                        // Onion and hostname addresses need a proxy/resolver
                        // lampo does not have, so they are skipped rather than
                        // counted as a failure to connect.
                        let host = match address {
                            ldk::ln::msgs::SocketAddress::TcpIpV4 { addr, port } => {
                                SocketAddr::from((std::net::Ipv4Addr::from(addr), port))
                            }
                            ldk::ln::msgs::SocketAddress::TcpIpV6 { addr, port } => {
                                SocketAddr::from((std::net::Ipv6Addr::from(addr), port))
                            }
                            other => {
                                log::warn!(target: "lampo::handler", "cannot dial `{other:?}` for `{node_id}`: unsupported address type");
                                continue;
                            }
                        };
                        log::info!(target: "lampo::handler", "connecting to `{node_id}` at `{host}` to deliver an onion message");
                        // `Ok` is shadowed by `lampo_common::error::Ok` (a
                        // function) in this module, so the variant has to be
                        // spelled out in pattern position.
                        match peer_manager.connect(node_id, host).await {
                            std::result::Result::Ok(()) => return,
                            Err(err) => {
                                log::warn!(target: "lampo::handler", "failed to connect to `{node_id}` at `{host}`: {err}");
                            }
                        }
                    }
                    log::error!(target: "lampo::handler", "no reachable address for `{node_id}`; an onion message to it will not be delivered");
                });
                Ok(())
            }
            ldk::events::Event::BumpTransaction(
                ldk::events::bump_transaction::BumpTransactionEvent::ChannelClose {
                    channel_id,
                    counterparty_node_id,
                    commitment_tx,
                    pending_htlcs,
                    ..
                },
            ) => {
                // For anchor channels -- lampo's default channel type -- LDK
                // does NOT broadcast the force-close commitment through the
                // `BroadcasterInterface`. It hands the fully-signed holder
                // commitment to the host inside this event and makes the host
                // the sole broadcaster. The old catch-all `_` arm dropped it in
                // a log line, so the channel balance stayed frozen on an
                // unspent funding output with no recovery path. Broadcast it
                // now so the funds can be swept once it confirms. A CPFP anchor
                // child to fee-bump the (near zero-fee) commitment is still
                // TODO, but the commitment must at least reach the mempool.
                log::warn!(
                    target: "lampo::handler",
                    "channel `{channel_id}` with `{counterparty_node_id}` force-closed: \
                     broadcasting local commitment tx `{}` ({} pending HTLC(s))",
                    commitment_tx.compute_txid(),
                    pending_htlcs.len(),
                );
                self.chain_manager.broadcast_transactions(&[(
                    &commitment_tx,
                    TransactionType::UnilateralClose {
                        counterparty_node_id,
                        channel_id,
                    },
                )]);
                Ok(())
            }
            ldk::events::Event::BumpTransaction(
                ldk::events::bump_transaction::BumpTransactionEvent::HTLCResolution { .. },
            ) => {
                // Zero-fee HTLC claims of anchor channels are likewise
                // event-delivered and host-broadcast. Full anchor-fee handling
                // is not implemented yet, so surface this loudly rather than
                // silently dropping it: unresolved in-flight HTLCs can lose
                // value once their CLTV deadlines pass.
                log::error!(
                    target: "lampo::handler",
                    "unhandled BumpTransaction::HTLCResolution: in-flight HTLC claims need \
                     manual broadcast to enforce CLTV deadlines"
                );
                Ok(())
            }
            _ => {
                log::warn!(target: "lampo::handler", "unhandled ldk event: {:?}", event);
                Ok(())
            }
        }
    }
}
