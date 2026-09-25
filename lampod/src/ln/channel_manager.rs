//! Channel Manager Implementation
use std::collections::{HashMap, HashSet};
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
    CHANNEL_MONITOR_PERSISTENCE_PRIMARY_NAMESPACE, CHANNEL_MONITOR_PERSISTENCE_SECONDARY_NAMESPACE,
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

/// Snapshot of the routing graph used by the gossip stall probe (issue #612).
#[derive(Debug, Clone, Copy)]
pub struct GossipGraphStats {
    pub graph_channels: usize,
    pub our_announced: usize,
    pub our_in_graph: usize,
    pub updated: usize,
    pub foreign_updated: usize,
    pub stalled: bool,
}

/// Classify graph entries for the stall probe.
///
/// A channel is "updated" only when both directions have a `channel_update`.
/// Announcement-only (or one-sided) entries are not routable (BOLT 7).
///
/// Stall means we have public ready channels whose SCID is missing from the
/// graph or lacks both updates. A missing *foreign* channel is an upstream
/// relay hole ([ldk-server#288](https://github.com/lightningdevkit/ldk-server/issues/288)),
/// not something reconnecting our counterparty can invent.
fn classify_gossip_graph<I>(our_scids: &HashSet<u64>, channels: I) -> GossipGraphStats
where
    I: IntoIterator<Item = (u64, bool, bool)>,
{
    let mut graph_channels = 0usize;
    let mut updated = 0usize;
    let mut foreign_updated = 0usize;
    let mut our_in_graph = 0usize;
    let mut our_updated = 0usize;
    for (scid, one_to_two, two_to_one) in channels {
        graph_channels += 1;
        let has_update = one_to_two && two_to_one;
        if has_update {
            updated += 1;
        }
        if our_scids.contains(&scid) {
            our_in_graph += 1;
            if has_update {
                our_updated += 1;
            }
        } else if has_update {
            foreign_updated += 1;
        }
    }
    let stalled =
        !our_scids.is_empty() && (our_in_graph < our_scids.len() || our_updated < our_scids.len());
    GossipGraphStats {
        graph_channels,
        our_announced: our_scids.len(),
        our_in_graph,
        updated,
        foreign_updated,
        stalled,
    }
}

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
    /// LDK gossip sync for this node's graph. Kept here so the event
    /// handler can re-query a peer when a channel becomes ready.
    gossip_sync: OnceLock<Arc<crate::P2PGossipSync>>,
    router: OnceLock<Arc<LampoRouter>>,
    /// Shared with the event handler; see [`FundingWaitState`].
    funding_wait_state: Mutex<HashMap<ChannelId, FundingWaitState>>,
    /// Explicit per-open fee requested by the caller (LND `sat_per_vbyte`),
    /// consumed by `FundingGenerationReady` when present; otherwise the
    /// cached estimate for `FeeTarget::ChannelFunding` is used.
    funding_fee_rates: Mutex<HashMap<ChannelId, u64>>,
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
            gossip_sync: OnceLock::new(),
            graph: OnceLock::new(),
            score: OnceLock::new(),
            router: OnceLock::new(),
            funding_wait_state: Mutex::new(HashMap::new()),
            funding_fee_rates: Mutex::new(HashMap::new()),
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

    /// Called once from `LampoPeerManager::init` so the same `P2PGossipSync`
    /// is the peer-manager `route_handler` *and* the background-processor
    /// gossip sync. A second instance would queue `GossipTimestampFilter`
    /// events that never reach the socket (issue #612).
    pub fn set_gossip_sync(&self, gossip_sync: Arc<crate::P2PGossipSync>) {
        if self.gossip_sync.set(gossip_sync).is_err() {
            log::warn!(target: "lampo-gossip", "gossip sync already initialized");
        }
    }

    pub fn gossip_sync(&self) -> Arc<crate::P2PGossipSync> {
        self.gossip_sync
            .get()
            .expect("gossip sync not initialized")
            .clone()
    }

    /// True when we have at least one public, ready channel whose SCID is
    /// missing from the routing graph or lacks both `channel_update`s.
    ///
    /// A node that connected before its own public channel was announced
    /// stays `RouteNotFound` until the peer re-dumps gossip. Restart heals
    /// in ~1s because it re-runs `P2PGossipSync::peer_connected` on the live
    /// handler. The peer-manager stall probe reconnects on this local gap
    /// (issue #612). Missing *foreign* channels are not a stall: that is
    /// an upstream relay hole.
    pub fn gossip_graph_looks_stalled(&self) -> bool {
        self.gossip_graph_stats().stalled
    }

    /// Snapshot of the routing graph used by the stall probe and logs.
    pub fn gossip_graph_stats(&self) -> GossipGraphStats {
        let our_scids: HashSet<u64> = self
            .manager()
            .list_channels()
            .into_iter()
            .filter(|channel| channel.is_announced && channel.is_channel_ready)
            .filter_map(|channel| channel.short_channel_id)
            .collect();
        let graph = self.graph();
        let view = graph.read_only();
        classify_gossip_graph(
            &our_scids,
            view.channels()
                .unordered_iter()
                .map(|(scid, info)| (*scid, info.one_to_two.is_some(), info.two_to_one.is_some())),
        )
    }

    pub fn handler(&self) -> Arc<LampoHandler> {
        self.handler.get().expect("handler not initialized").clone()
    }

    pub async fn listen(self: Arc<Self>) -> error::Result<()> {
        if self.is_restarting()? {
            self.restart()?;
        } else if self.monitors_without_manager()? {
            // A wiped manager with leftover monitors is not a new node. Starting
            // fresh under the same identity drops the channel while the
            // counterparty still has it.
            error::bail!(
                "channel monitors exist but the channel manager is missing; refusing to start a new node"
            );
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
        let mut config = self.conf.ldk_conf_with_async_role();
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
        if let Some(sat_per_vbyte) = open_channel.sat_per_vbyte {
            self.funding_fee_rates
                .lock()
                .map_err(|_| error::anyhow!("funding fee-rate lock is poisoned"))?
                .insert(temp_channel_id, sat_per_vbyte);
        }

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

        if channel.force {
            self.manager()
                .force_close_broadcasting_latest_txn(
                    &channel_id,
                    &node_id,
                    "Force close requested by RPC client".into(),
                )
                .map_err(|err| error::anyhow!("{:?}", err))?;
        } else {
            self.manager()
                .close_channel(&channel_id, &node_id)
                .map_err(|err| error::anyhow!("{:?}", err))?;
        }
        Ok(())
    }

    pub fn take_funding_fee_rate(
        &self,
        temporary_channel_id: &ChannelId,
    ) -> error::Result<Option<u64>> {
        Ok(self
            .funding_fee_rates
            .lock()
            .map_err(|_| error::anyhow!("funding fee-rate lock is poisoned"))?
            .remove(temporary_channel_id))
    }

    /// Whether persisted channel state exists to restore, whichever backend
    /// holds it. A database-backed node checked a filesystem path here once,
    /// and restarted as a brand-new node while its monitors sat in the store.
    fn monitors_without_manager(&self) -> error::Result<bool> {
        let monitors = self.persister.list(
            CHANNEL_MONITOR_PERSISTENCE_PRIMARY_NAMESPACE,
            CHANNEL_MONITOR_PERSISTENCE_SECONDARY_NAMESPACE,
        )?;
        Ok(!monitors.is_empty())
    }

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
            self.conf.ldk_conf_with_async_role(),
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
            self.conf.ldk_conf_with_async_role(),
            chain_params,
            now.as_secs() as u32,
        ));
        self.channeld
            .set(channeld)
            .unwrap_or_else(|_| panic!("channel manager already initialized"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scids(ids: &[u64]) -> HashSet<u64> {
        ids.iter().copied().collect()
    }

    #[test]
    fn empty_graph_is_not_stalled_without_our_channels() {
        let stats = classify_gossip_graph(&scids(&[]), std::iter::empty());
        assert!(!stats.stalled);
        assert_eq!(stats.graph_channels, 0);
    }

    #[test]
    fn our_ready_channel_missing_from_graph_is_stalled() {
        let stats = classify_gossip_graph(&scids(&[1]), std::iter::empty());
        assert!(stats.stalled);
        assert_eq!(stats.our_announced, 1);
        assert_eq!(stats.our_in_graph, 0);
    }

    #[test]
    fn announcement_only_own_channel_is_stalled() {
        let stats = classify_gossip_graph(&scids(&[1]), [(1, false, false)]);
        assert!(stats.stalled);
        assert_eq!(stats.our_in_graph, 1);
        assert_eq!(stats.updated, 0);
    }

    #[test]
    fn one_sided_own_channel_is_stalled() {
        let stats = classify_gossip_graph(&scids(&[1]), [(1, true, false)]);
        assert!(stats.stalled);
        assert_eq!(stats.updated, 0);
    }

    #[test]
    fn both_updates_on_own_channel_is_not_stalled() {
        let stats = classify_gossip_graph(&scids(&[1]), [(1, true, true)]);
        assert!(!stats.stalled);
        assert_eq!(stats.our_in_graph, 1);
        assert_eq!(stats.updated, 1);
        assert_eq!(stats.foreign_updated, 0);
    }

    #[test]
    fn missing_foreign_channel_is_not_a_local_stall() {
        // lp2 knows c3 (ours, both updates) but not c2. That is ldk-server#288,
        // not something reconnecting lk1 invents.
        let stats = classify_gossip_graph(&scids(&[3]), [(3, true, true)]);
        assert!(!stats.stalled);
        assert_eq!(stats.foreign_updated, 0);
    }

    #[test]
    fn foreign_updated_channel_is_counted_not_stalled() {
        let stats = classify_gossip_graph(&scids(&[1]), [(1, true, true), (2, true, true)]);
        assert!(!stats.stalled);
        assert_eq!(stats.graph_channels, 2);
        assert_eq!(stats.foreign_updated, 1);
    }
}
