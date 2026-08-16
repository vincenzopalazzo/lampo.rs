//! Channel Manager Implementation
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use lampo_common::backend::Backend;
use lampo_common::bitcoin::{BlockHash, Transaction};
use lampo_common::conf::LampoConf;
use lampo_common::error;
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
use lampo_common::ldk::ln::channelmanager::{ChainParameters, ChannelManagerReadArgs};
use lampo_common::ldk::onion_message::messenger::DefaultMessageRouter;
use lampo_common::ldk::routing::gossip::NetworkGraph;
use lampo_common::ldk::routing::router::DefaultRouter;
use lampo_common::ldk::routing::scoring::{
    ProbabilisticScorer, ProbabilisticScoringDecayParameters, ProbabilisticScoringFeeParameters,
};
use lampo_common::ldk::sign::{InMemorySigner, NodeSigner};
use lampo_common::ldk::util::persist::read_channel_monitors;
use lampo_common::ldk::util::ser::ReadableArgs;
use lampo_common::model::request;
use lampo_common::model::response::{self, Channel, Channels};
use lampo_common::types::LampoChannel;
use lampo_common::types::LampoGraph;
use lampo_common::types::LampoRouter;
use lampo_common::types::LampoScorer;
use lampo_common::types::{ChannelId, LampoArcChannelManager, LampoChainMonitor};

use crate::actions::handler::LampoHandler;
use crate::async_run;
use crate::chain::{LampoChainManager, WalletManager};
use crate::persistence::LampoPersistence;
use crate::utils::logger::LampoLogger;

pub struct LampoChannelManager {
    monitor: OnceLock<Arc<LampoChainMonitor>>,
    wallet_manager: Arc<dyn WalletManager>,
    persister: Arc<LampoPersistence>,
    graph: OnceLock<Arc<LampoGraph>>,
    score: OnceLock<Arc<Mutex<LampoScorer>>>,
    handler: OnceLock<Arc<LampoHandler>>,
    router: OnceLock<Arc<LampoRouter>>,
    funding_fee_rates: Mutex<HashMap<ChannelId, u64>>,

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
        persister: Arc<LampoPersistence>,
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
            funding_fee_rates: Mutex::new(HashMap::new()),
        }
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
        Ok(())
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
                let network_graph_path = format!("{}/network_graph", self.conf.path());
                let network_graph = self.read_network(Path::new(&network_graph_path));

                let scorer_path = format!("{}/scorer", self.conf.path());
                let scorer = Arc::new(Mutex::new(
                    self.read_scorer(Path::new(&scorer_path), &network_graph),
                ));

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
        path: &Path,
        graph: &Arc<LampoGraph>,
    ) -> ProbabilisticScorer<Arc<LampoGraph>, Arc<LampoLogger>> {
        let params = ProbabilisticScoringDecayParameters::default();
        if let Ok(file) = File::open(path) {
            let args = (params, Arc::clone(graph), self.logger.clone());
            if let Ok(scorer) = ProbabilisticScorer::read(&mut BufReader::new(file), args) {
                return scorer;
            }
        }
        ProbabilisticScorer::new(params, graph.clone(), self.logger.clone())
    }

    pub(crate) fn read_network(&self, path: &Path) -> Arc<LampoGraph> {
        if let Ok(file) = File::open(path) {
            if let Ok(graph) = NetworkGraph::read(&mut BufReader::new(file), self.logger.clone()) {
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
        let push_msat = open_channel.push_msat.unwrap_or(0);
        let temporary_channel_id = self
            .manager()
            .create_channel(
                open_channel.node_id()?,
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
                .insert(temporary_channel_id, sat_per_vbyte);
        }

        // Wait for SendRawTransaction or FundingChannelFailed to be received
        let mut events = self.handler().events();
        let tx: Option<Transaction> = loop {
            let event = events
                .recv()
                .await
                .ok_or(error::anyhow!("Channel funding: no event received"))?;

            match event {
                Event::OnChain(OnChainEvent::SendRawTransaction(tx)) => {
                    break Some(tx);
                }
                Event::OnChain(OnChainEvent::FundingChannelFailed(reason)) => {
                    return Err(error::anyhow!("{}", reason));
                }
                _ => {}
            }
        };

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

    pub fn is_restarting(&self) -> error::Result<bool> {
        Ok(Path::exists(Path::new(&format!(
            "{}/manager",
            self.conf.path()
        ))))
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
        let mut channel_manager_file = File::open(format!("{}/manager", self.conf.path()))?;
        let (_, channel_manager) =
            <(BlockLocator, LampoChannel)>::read(&mut channel_manager_file, read_args)
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
