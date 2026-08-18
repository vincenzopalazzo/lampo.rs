//! Channel Manager Implementation
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use lampo_common::backend::Backend;
use lampo_common::bitcoin::{BlockHash, Transaction};
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::onchain::OnChainEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::json::de;
use lampo_common::keys::LampoKeysManager;
use lampo_common::ldk::block_sync::BlockSource;
use lampo_common::ldk::chain::chaininterface::{BroadcasterInterface, FeeEstimator};
use lampo_common::ldk::chain::chainmonitor::ChainMonitor;
use lampo_common::ldk::chain::channelmonitor::ChannelMonitor;
use lampo_common::ldk::chain::{BlockLocator, Watch};
use lampo_common::ldk::io::Cursor;
use lampo_common::ldk::ln::channelmanager::{ChainParameters, ChannelManagerReadArgs};
use lampo_common::ldk::onion_message::messenger::DefaultMessageRouter;
use lampo_common::ldk::routing::gossip::NetworkGraph;
use lampo_common::ldk::routing::router::DefaultRouter;
use lampo_common::ldk::routing::scoring::{
    ProbabilisticScorer, ProbabilisticScoringDecayParameters, ProbabilisticScoringFeeParameters,
};
use lampo_common::ldk::sign::{InMemorySigner, NodeSigner};
use lampo_common::ldk::util::persist::{
    read_channel_monitors, KVStoreSync, CHANNEL_MANAGER_PERSISTENCE_KEY,
    CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE, CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
    NETWORK_GRAPH_PERSISTENCE_KEY, NETWORK_GRAPH_PERSISTENCE_PRIMARY_NAMESPACE,
    NETWORK_GRAPH_PERSISTENCE_SECONDARY_NAMESPACE, OUTPUT_SWEEPER_PERSISTENCE_KEY,
    OUTPUT_SWEEPER_PERSISTENCE_PRIMARY_NAMESPACE, OUTPUT_SWEEPER_PERSISTENCE_SECONDARY_NAMESPACE,
    SCORER_PERSISTENCE_KEY, SCORER_PERSISTENCE_PRIMARY_NAMESPACE,
    SCORER_PERSISTENCE_SECONDARY_NAMESPACE,
};
use lampo_common::ldk::util::ser::ReadableArgs;
use lampo_common::ldk::util::sweep::OutputSweeper;
use lampo_common::model::request;
use lampo_common::model::response::{self, Channel, Channels};
use lampo_common::persist::{LampoAsyncPersistence, LampoPersistenceBackend};
use lampo_common::types::LampoChannel;
use lampo_common::types::LampoGraph;
use lampo_common::types::LampoRouter;
use lampo_common::types::LampoScorer;
use lampo_common::types::LampoSweeper;
use lampo_common::types::{ChannelId, LampoArcChannelManager, LampoChainMonitor};

use crate::actions::handler::LampoHandler;
use crate::async_run;
use crate::chain::{LampoChainManager, WalletManager};
use crate::utils::logger::LampoLogger;

/// How long `open_channel` waits for the funding transaction to be broadcast
/// before giving up. LDK reaps a stalled unfunded channel after roughly a
/// minute of ping ticks; this outer bound guarantees the request always
/// returns even if the peer stalls in a state that emits no terminal event.
const FUNDING_WAIT_TIMEOUT_SECS: u64 = 120;

/// Coordinates the `open_channel` waiter with `FundingGenerationReady` so a
/// timeout cannot force-close a channel whose funding LDK already accepted
/// (and vice versa: the producer must not hand off after the waiter abandoned).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FundingWaitState {
    /// Waiter timed out; producer must not call `funding_transaction_generated`.
    Abandoned,
    /// Producer handed funding to LDK; waiter must not force-close.
    Accepted,
}

pub struct LampoChannelManager {
    monitor: OnceLock<Arc<LampoChainMonitor>>,
    wallet_manager: Arc<dyn WalletManager>,
    persister: Arc<dyn LampoPersistenceBackend>,
    graph: OnceLock<Arc<LampoGraph>>,
    score: OnceLock<Arc<Mutex<LampoScorer>>>,
    handler: OnceLock<Arc<LampoHandler>>,
    router: OnceLock<Arc<LampoRouter>>,
    /// Shared with the event handler; see [`FundingWaitState`].
    funding_wait_state: Mutex<HashMap<ChannelId, FundingWaitState>>,
    /// Restored (or freshly created) output sweeper, paired with the best
    /// block its persisted state was last synced to so the chain backend can
    /// catch it up independently of the channel manager.
    sweeper: OnceLock<(BlockLocator, Arc<LampoSweeper>)>,

    pub(crate) onchain: Arc<LampoChainManager>,
    pub(crate) conf: LampoConf,
    channeld: OnceLock<Arc<LampoChannel>>,
    pub(crate) logger: Arc<LampoLogger>,
}

impl LampoChannelManager {
    pub fn new(
        conf: &LampoConf,
        logger: Arc<LampoLogger>,
        onchain: Arc<LampoChainManager>,
        wallet_manager: Arc<dyn WalletManager>,
        persister: Arc<dyn LampoPersistenceBackend>,
    ) -> Self {
        LampoChannelManager {
            conf: conf.to_owned(),
            monitor: OnceLock::new(),
            onchain,
            channeld: OnceLock::new(),
            wallet_manager,
            logger,
            persister,
            handler: OnceLock::new(),
            graph: OnceLock::new(),
            score: OnceLock::new(),
            router: OnceLock::new(),
            funding_wait_state: Mutex::new(HashMap::new()),
            sweeper: OnceLock::new(),
        }
    }

    /// Hand funding to LDK only if the waiter has not already abandoned.
    /// Holds the wait-state lock across the LDK call so timeout cleanup cannot
    /// race the handoff.
    pub(crate) fn funding_transaction_generated_if_waiting(
        &self,
        temporary_channel_id: ChannelId,
        counterparty_node_id: lampo_common::types::NodeId,
        transaction: Transaction,
    ) -> error::Result<bool> {
        let mut state = self
            .funding_wait_state
            .lock()
            .expect("funding wait state poisoned");
        if matches!(
            state.get(&temporary_channel_id),
            Some(FundingWaitState::Abandoned)
        ) {
            return Ok(false);
        }
        self.manager()
            .funding_transaction_generated(temporary_channel_id, counterparty_node_id, transaction)
            .map_err(|err| error::anyhow!("{:?}", err))?;
        state.insert(temporary_channel_id, FundingWaitState::Accepted);
        Ok(true)
    }

    /// Returns `true` when the waiter should force-close (handoff not yet
    /// accepted). Marks the open as abandoned so a concurrent producer skips
    /// `funding_transaction_generated`.
    pub(crate) fn abandon_funding_wait_if_unaccepted(
        &self,
        temporary_channel_id: ChannelId,
    ) -> bool {
        let mut state = self
            .funding_wait_state
            .lock()
            .expect("funding wait state poisoned");
        if matches!(
            state.get(&temporary_channel_id),
            Some(FundingWaitState::Accepted)
        ) {
            return false;
        }
        state.insert(temporary_channel_id, FundingWaitState::Abandoned);
        true
    }

    pub(crate) fn clear_funding_wait(&self, temporary_channel_id: &ChannelId) {
        self.funding_wait_state
            .lock()
            .expect("funding wait state poisoned")
            .remove(temporary_channel_id);
    }

    pub fn set_handler(&self, handler: Arc<LampoHandler>) {
        self.handler
            .set(handler)
            .unwrap_or_else(|_| panic!("handler already initialized"));
    }

    pub fn handler(&self) -> Arc<LampoHandler> {
        self.handler.get().expect("handler not initialized").clone()
    }

    pub async fn listen(self: Arc<Self>) -> error::Result<()> {
        if self.is_restarting()? {
            self.restart()?;
        } else {
            self.start().await?;
        }
        self.init_sweeper()?;
        Ok(())
    }

    /// Broadcaster, fee estimator, and keys manager shared by restore and
    /// first-time sweeper construction. Spender and change destination are
    /// the same keys manager, as in ldk-node.
    fn sweeper_deps(
        &self,
    ) -> (
        Arc<dyn BroadcasterInterface + Send + Sync>,
        Arc<dyn FeeEstimator + Send + Sync>,
        Arc<LampoKeysManager>,
    ) {
        (
            self.onchain.clone(),
            self.onchain.clone(),
            self.wallet_manager.ldk_keys().keys_manager.clone(),
        )
    }

    /// Restore a previously persisted [`LampoSweeper`], including its tracked
    /// outputs and last synced block.
    fn restore_sweeper(
        &self,
        bytes: Vec<u8>,
        broadcaster: Arc<dyn BroadcasterInterface + Send + Sync>,
        fee_estimator: Arc<dyn FeeEstimator + Send + Sync>,
        keys_manager: Arc<LampoKeysManager>,
    ) -> error::Result<(BlockLocator, LampoSweeper)> {
        // ReadableArgs order: broadcaster, fee estimator, filter, spender,
        // change destination, kv store, logger.
        <(BlockLocator, LampoSweeper)>::read(
            &mut std::io::Cursor::new(bytes),
            (
                broadcaster,
                fee_estimator,
                None,
                keys_manager.clone(),
                keys_manager,
                LampoAsyncPersistence::new(self.persister.clone()),
                self.logger.clone(),
            ),
        )
        .map_err(|err| error::anyhow!("failed to read the sweeper state: {err}"))
    }

    /// Build the [`LampoSweeper`], restoring its persisted state when present.
    fn init_sweeper(&self) -> error::Result<()> {
        let (broadcaster, fee_estimator, keys_manager) = self.sweeper_deps();
        let persisted = KVStoreSync::read(
            &*self.persister,
            OUTPUT_SWEEPER_PERSISTENCE_PRIMARY_NAMESPACE,
            OUTPUT_SWEEPER_PERSISTENCE_SECONDARY_NAMESPACE,
            OUTPUT_SWEEPER_PERSISTENCE_KEY,
        );
        let (best_block, sweeper) = match persisted {
            Ok(bytes) => self.restore_sweeper(bytes, broadcaster, fee_estimator, keys_manager)?,
            Err(err) if err.kind() == lampo_common::ldk::io::ErrorKind::NotFound => {
                let best_block = self.manager().current_best_block();
                let sweeper = OutputSweeper::new(
                    best_block.clone(),
                    broadcaster,
                    fee_estimator,
                    None,
                    keys_manager.clone(),
                    keys_manager,
                    LampoAsyncPersistence::new(self.persister.clone()),
                    self.logger.clone(),
                );
                (best_block, sweeper)
            }
            Err(err) => error::bail!("failed to read the sweeper state: {err}"),
        };
        self.sweeper
            .set((best_block, Arc::new(sweeper)))
            .unwrap_or_else(|_| panic!("sweeper already initialized"));
        Ok(())
    }

    pub fn sweeper(&self) -> Arc<LampoSweeper> {
        self.sweeper
            .get()
            .expect("sweeper not initialized")
            .1
            .clone()
    }

    pub fn sweeper_best_block(&self) -> BlockLocator {
        self.sweeper
            .get()
            .expect("sweeper not initialized")
            .0
            .clone()
    }

    fn build_channel_monitor(&self) -> LampoChainMonitor {
        let keys = self.wallet_manager.ldk_keys().keys_manager.clone();
        ChainMonitor::new(
            // FIXME: this is needed when use esplora or electrum
            None,
            self.onchain.clone(),
            self.logger.clone(),
            self.onchain.clone(),
            self.persister.clone(),
            keys.clone(),
            keys.get_peer_storage_key(),
            // `deferred`: lampo uses synchronous filesystem persistence.
            false,
        )
    }

    pub fn chain_monitor(&self) -> Arc<LampoChainMonitor> {
        self.monitor
            .get()
            .expect("chain monitor not initialized")
            .clone()
    }

    pub fn wallet_manager(&self) -> Arc<dyn WalletManager> {
        self.wallet_manager.clone()
    }

    pub fn manager(&self) -> Arc<LampoChannel> {
        self.channeld
            .get()
            .expect("channel manager not initialized")
            .clone()
    }

    pub fn list_channels(&self) -> Channels {
        let channels: Vec<Channel> = self
            .manager()
            .list_channels()
            .into_iter()
            .map(|channel| Channel {
                channel_id: channel.channel_id.to_string(),
                short_channel_id: channel.short_channel_id,
                peer_id: channel.counterparty.node_id.to_string(),
                peer_alias: None,
                ready: channel.is_channel_ready,
                amount: channel.channel_value_satoshis,
                amount_msat: channel.next_outbound_htlc_limit_msat,
                public: channel.is_announced,
                available_balance_for_send_msat: channel.outbound_capacity_msat,
                available_balance_for_recv_msat: channel.inbound_capacity_msat,
            })
            .collect();
        Channels { channels }
    }

    pub fn get_channel_monitors(&self) -> error::Result<Vec<ChannelMonitor<InMemorySigner>>> {
        let keys = self.wallet_manager.ldk_keys().inner();
        let mut monitors = read_channel_monitors(self.persister.clone(), keys.clone(), keys)?;
        let mut channel_monitors = Vec::new();
        for (_, monitor) in monitors.drain(..) {
            channel_monitors.push(monitor);
        }
        Ok(channel_monitors)
    }

    pub fn graph(&self) -> Arc<LampoGraph> {
        self.graph
            .get()
            .expect("network graph not initialized")
            .clone()
    }

    pub fn scorer(&self) -> Arc<Mutex<LampoScorer>> {
        self.score.get().expect("scorer not initialized").clone()
    }

    // FIXME: Step 11: Optional: Initialize the NetGraphMsgHandler
    pub fn network_graph(
        &self,
    ) -> Arc<
        DefaultRouter<
            Arc<LampoGraph>,
            Arc<LampoLogger>,
            Arc<LampoKeysManager>,
            Arc<Mutex<LampoScorer>>,
            ProbabilisticScoringFeeParameters,
            LampoScorer,
        >,
    > {
        self.router
            .get_or_init(|| {
                let network_graph = self.read_network();
                let scorer = Arc::new(Mutex::new(self.read_scorer(&network_graph)));

                self.graph
                    .set(network_graph.clone())
                    .unwrap_or_else(|_| panic!("graph OnceLock already initialized"));
                self.score
                    .set(scorer.clone())
                    .unwrap_or_else(|_| panic!("score OnceLock already initialized"));
                Arc::new(DefaultRouter::new(
                    network_graph,
                    self.logger.clone(),
                    self.wallet_manager.ldk_keys().keys_manager.clone(),
                    scorer,
                    ProbabilisticScoringFeeParameters::default(),
                ))
            })
            .clone()
    }

    pub(crate) fn read_scorer(
        &self,
        graph: &Arc<LampoGraph>,
    ) -> ProbabilisticScorer<Arc<LampoGraph>, Arc<LampoLogger>> {
        let params = ProbabilisticScoringDecayParameters::default();
        if let Ok(buf) = self.persister.read(
            SCORER_PERSISTENCE_PRIMARY_NAMESPACE,
            SCORER_PERSISTENCE_SECONDARY_NAMESPACE,
            SCORER_PERSISTENCE_KEY,
        ) {
            let args = (params, Arc::clone(graph), self.logger.clone());
            if let Ok(scorer) = ProbabilisticScorer::read(&mut Cursor::new(buf), args) {
                return scorer;
            }
        }
        ProbabilisticScorer::new(params, graph.clone(), self.logger.clone())
    }

    pub(crate) fn read_network(&self) -> Arc<LampoGraph> {
        if let Ok(buf) = self.persister.read(
            NETWORK_GRAPH_PERSISTENCE_PRIMARY_NAMESPACE,
            NETWORK_GRAPH_PERSISTENCE_SECONDARY_NAMESPACE,
            NETWORK_GRAPH_PERSISTENCE_KEY,
        ) {
            if let Ok(graph) = NetworkGraph::read(&mut Cursor::new(buf), self.logger.clone()) {
                return Arc::new(graph);
            }
        }
        Arc::new(NetworkGraph::new(self.conf.network, self.logger.clone()))
    }

    pub async fn open_channel(
        &self,
        open_channel: request::OpenChannel,
    ) -> error::Result<response::OpenChannel> {
        // The caller decides whether this channel is announced. Passing the
        // global config unmodified here silently dropped `public`: LDK's
        // `announce_for_forwarding` defaults to false, so every channel came
        // up unannounced no matter what the caller asked for, and a payment
        // could never route *through* a lampo node — gossip never learned
        // its channels existed.
        let mut config = self.conf.ldk_conf.clone();
        config.channel_handshake_config.announce_for_forwarding = open_channel.public;
        let peer_id = open_channel.node_id()?;
        let push_msat = open_channel.push_msat.unwrap_or(0);
        // Subscribe *before* `create_channel`: a fast peer can finish
        // negotiation on another runtime thread and emit `FundingChannelEnd`
        // before a post-create subscription would see it, leaving the handoff
        // flag false and the timeout path force-closing an already-funded
        // channel.
        let mut events = self.handler().events();
        let temp_channel_id = self
            .manager()
            .create_channel(
                peer_id,
                open_channel.amount,
                push_msat,
                0,
                None,
                Some(config),
            )
            .map_err(|err| error::anyhow!("{:?}", err))?;

        // Wait for *this* channel's funding transaction to be broadcast, or
        // for the open to fail. The event bus is process-wide: any
        // `SendRawTransaction` (including unilateral-close / bump broadcasts)
        // or any `FundingChannelFailed` would otherwise complete or abort an
        // unrelated waiter. Close events are matched on the temporary channel
        // id returned by `create_channel` for the same reason. Without a
        // terminal case and an overall timeout the request task blocks on
        // `recv().await` forever, leaking its actix task, socket and event-bus
        // subscription; an unauthenticated flood of such requests exhausts the
        // process fd table and takes the node down.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(FUNDING_WAIT_TIMEOUT_SECS);
        let mut expected_funding_txid = None;
        let mut funding_handed_to_ldk = false;
        let mut early_broadcast: Option<Transaction> = None;
        let tx: Option<Transaction> = 'wait: loop {
            // Apply one event. `None` keeps waiting; `Some` ends the loop.
            let apply = |event: Event,
                         expected: &mut Option<lampo_common::bitcoin::Txid>,
                         handed: &mut bool,
                         early: &mut Option<Transaction>|
             -> Option<error::Result<Option<Transaction>>> {
                match event {
                    Event::Lightning(LightningEvent::FundingChannelEnd {
                        temporary_channel_id,
                        funding_transaction,
                        ..
                    }) if temporary_channel_id == temp_channel_id => {
                        let txid = funding_transaction.compute_txid();
                        *expected = Some(txid);
                        *handed = true;
                        // Broadcast can race ahead of this event on a fast
                        // peer; accept a buffered matching SendRawTransaction.
                        if early.as_ref().is_some_and(|tx| tx.compute_txid() == txid) {
                            return Some(Ok(early.take()));
                        }
                        None
                    }
                    Event::OnChain(OnChainEvent::SendRawTransaction(tx)) => {
                        if *expected == Some(tx.compute_txid()) {
                            Some(Ok(Some(tx)))
                        } else if expected.is_none() {
                            *early = Some(tx);
                            None
                        } else {
                            None
                        }
                    }
                    Event::OnChain(OnChainEvent::FundingChannelFailed {
                        temporary_channel_id: Some(channel_id),
                        reason,
                        ..
                    }) if channel_id == temp_channel_id.to_string() => {
                        Some(Err(error::anyhow!("{}", reason)))
                    }
                    Event::OnChain(OnChainEvent::FundingChannelFailed {
                        txid: Some(failed_txid),
                        reason,
                        ..
                    }) if *expected == Some(failed_txid) => {
                        // Handoff already succeeded; broadcast RPC errors are
                        // ambiguous (backend may have accepted the tx) and LDK
                        // may still rebroadcast. Do not present this as a
                        // safely-retryable open failure.
                        Some(Err(error::anyhow!(
                            "channel funding broadcast reported failure after handoff ({reason}); channel left open (broadcast may still be pending)"
                        )))
                    }
                    Event::Lightning(LightningEvent::CloseChannelEvent {
                        channel_id,
                        message,
                        ..
                    }) if channel_id == temp_channel_id.to_string() => Some(Err(error::anyhow!(
                        "channel closed before funding: {message}"
                    ))),
                    _ => None,
                }
            };

            if tokio::time::Instant::now() >= deadline {
                // Drain events that arrived as the deadline fired so a queued
                // `FundingChannelEnd` cannot be missed before force-close.
                while let Ok(event) = events.try_recv() {
                    if let Some(done) = apply(
                        event,
                        &mut expected_funding_txid,
                        &mut funding_handed_to_ldk,
                        &mut early_broadcast,
                    ) {
                        self.clear_funding_wait(&temp_channel_id);
                        break 'wait done?;
                    }
                }
                // Producer-owned wait state serializes with handoff: if LDK
                // already accepted funding, do not force-close; if not, mark
                // Abandoned so a concurrent producer skips handoff.
                if funding_handed_to_ldk
                    || !self.abandon_funding_wait_if_unaccepted(temp_channel_id)
                {
                    return Err(error::anyhow!(
                        "channel funding broadcast still pending after {FUNDING_WAIT_TIMEOUT_SECS}s for peer {peer_id}; channel left open"
                    ));
                }
                if let Err(err) = self.manager().force_close_broadcasting_latest_txn(
                    &temp_channel_id,
                    &peer_id,
                    format!("funding wait timed out after {FUNDING_WAIT_TIMEOUT_SECS}s"),
                ) {
                    log::warn!(
                        target: "lampo",
                        "failed to abandon timed-out channel `{temp_channel_id}` with `{peer_id}`: {err:?}"
                    );
                    // Keep the `Abandoned` tombstone: a late
                    // `FundingGenerationReady` must not hand funding to LDK
                    // after we already reported timeout to the caller.
                    return Err(error::anyhow!(
                        "channel funding timed out after {FUNDING_WAIT_TIMEOUT_SECS}s waiting for peer {peer_id}"
                    ));
                }
                self.clear_funding_wait(&temp_channel_id);
                return Err(error::anyhow!(
                    "channel funding timed out after {FUNDING_WAIT_TIMEOUT_SECS}s waiting for peer {peer_id}"
                ));
            }

            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Some(event)) => {
                    if let Some(done) = apply(
                        event,
                        &mut expected_funding_txid,
                        &mut funding_handed_to_ldk,
                        &mut early_broadcast,
                    ) {
                        self.clear_funding_wait(&temp_channel_id);
                        break 'wait done?;
                    }
                }
                Ok(None) => {
                    self.clear_funding_wait(&temp_channel_id);
                    return Err(error::anyhow!("Channel funding: no event received"));
                }
                Err(_) => {
                    // Deadline elapsed while waiting; next iteration drains.
                }
            }
        };

        self.clear_funding_wait(&temp_channel_id);
        let txid = tx.as_ref().map(|tx| tx.txid());

        Ok(response::OpenChannel {
            node_id: open_channel.node_id,
            amount: open_channel.amount,
            public: open_channel.public,
            push_msat: open_channel.push_msat.unwrap_or(0),
            to_self_delay: 2016,
            tx,
            txid,
        })
    }

    pub fn close_channel(&self, channel: request::CloseChannel) -> error::Result<()> {
        let channel_id = channel.channel_id()?;
        let node_id = channel.counterpart_node_id()?;

        self.manager()
            .close_channel(&channel_id, &node_id)
            .map_err(|err| error::anyhow!("{:?}", err))?;
        Ok(())
    }

    /// Whether persisted channel state exists to restore, whichever backend
    /// holds it. A database-backed node checked a filesystem path here once,
    /// and restarted as a brand-new node while its monitors sat in the store.
    pub fn is_restarting(&self) -> error::Result<bool> {
        match self.persister.read(
            CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
            CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
            CHANNEL_MANAGER_PERSISTENCE_KEY,
        ) {
            Ok(_) => Ok(true),
            Err(err) if err.kind() == lampo_common::ldk::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err.into()),
        }
    }

    pub fn restart(&self) -> error::Result<()> {
        let monitor = self.build_channel_monitor();
        self.monitor
            .set(Arc::new(monitor))
            .unwrap_or_else(|_| panic!("chain monitor already initialized"));

        let _ = self.network_graph();
        let monitors = self.get_channel_monitors()?;

        let default_message_router = DefaultMessageRouter::new(
            self.graph(),
            self.wallet_manager.ldk_keys().keys_manager.clone(),
        );
        let default_message_router = Arc::new(default_message_router);
        let read_args = ChannelManagerReadArgs::new(
            self.wallet_manager.ldk_keys().keys_manager.clone(),
            self.wallet_manager.ldk_keys().keys_manager.clone(),
            self.wallet_manager.ldk_keys().keys_manager.clone(),
            self.onchain.clone() as Arc<dyn FeeEstimator + Send + Sync>,
            self.chain_monitor(),
            self.onchain.clone() as Arc<dyn BroadcasterInterface + Send + Sync>,
            self.router.get().expect("router not initialized").clone(),
            default_message_router,
            self.logger.clone(),
            self.conf.ldk_conf.clone(),
            monitors.iter().collect(),
        );
        let manager_bytes = self.persister.read(
            CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
            CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
            CHANNEL_MANAGER_PERSISTENCE_KEY,
        )?;
        let (_, channel_manager) =
            <(BlockLocator, LampoChannel)>::read(&mut Cursor::new(manager_bytes), read_args)
                .map_err(|err| error::anyhow!("{err}"))?;

        // Move the persisted channel monitors into the `ChainMonitor`, as
        // required by LDK when restoring a node from disk (see the
        // `ChannelManagerReadArgs` documentation). Without this the monitor
        // of every channel that predates the restart is missing from the
        // `ChainMonitor`, so the first monitor update fails with
        // `no such monitor registered` and the restored channels are left
        // silently broken: payments stall without a failure event and the
        // peers keep reconnecting without making progress (issue #563).
        for monitor in monitors {
            let channel_id = monitor.channel_id();
            match self.chain_monitor().watch_channel(channel_id, monitor) {
                Ok(status) => log::info!(
                    target: "lampod",
                    "restored channel monitor for channel `{channel_id}` ({status:?})"
                ),
                Err(()) => log::error!(
                    target: "lampod",
                    "unable to register the persisted channel monitor for channel `{channel_id}`"
                ),
            }
        }

        self.channeld
            .set(Arc::new(channel_manager))
            .unwrap_or_else(|_| panic!("channel manager already initialized"));
        Ok(())
    }

    pub async fn start(&self) -> error::Result<()> {
        let (block_hash, block_height) = self.onchain.get_best_block().await
        .map_err(|err| error::anyhow!("Failed to connect to bitcoind: {:?}. Please ensure bitcoind is running and accessible.", err))?;
        let chain_params = ChainParameters {
            network: self.conf.network,
            // FIXME: the default height could be dangerous here
            best_block: BlockLocator::new(block_hash, block_height.unwrap_or_default()),
        };

        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?;
        let monitor = self.build_channel_monitor();
        self.monitor
            .set(Arc::new(monitor))
            .unwrap_or_else(|_| panic!("chain monitor already initialized"));

        // network_graph() lazily initializes the graph, scorer, and router
        let network_graph = self.network_graph();
        let default_message_router = DefaultMessageRouter::new(
            self.graph(),
            self.wallet_manager.ldk_keys().keys_manager.clone(),
        );
        let default_message_router = Arc::new(default_message_router);

        let keymanagers = self.wallet_manager.ldk_keys().keys_manager.clone();
        let channeld = Arc::new(LampoArcChannelManager::new(
            self.onchain.clone(),
            self.chain_monitor(),
            self.onchain.clone(),
            network_graph,
            default_message_router.clone(),
            self.logger.clone(),
            keymanagers.clone(),
            keymanagers.clone(),
            keymanagers,
            self.conf.ldk_conf.clone(),
            chain_params,
            now.as_secs() as u32,
        ));
        self.channeld
            .set(channeld)
            .unwrap_or_else(|_| panic!("channel manager already initialized"));
        Ok(())
    }
}
