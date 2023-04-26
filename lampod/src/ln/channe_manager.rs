//! Channel Manager Implementation

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::{Arc, Mutex};

use bitcoin::locktime::Height;
use bitcoin::BlockHash;
use lightning::chain::chainmonitor::ChainMonitor;
use lightning::chain::keysinterface::EntropySource;
use lightning::chain::keysinterface::InMemorySigner;
use lightning::chain::Watch;
use lightning::chain::{BestBlock, Filter};
use lightning::ln::channelmanager::{ChainParameters, SimpleArcChannelManager};
use lightning::routing::gossip::NetworkGraph;
use lightning::routing::router::DefaultRouter;
use lightning::routing::scoring::{ProbabilisticScorer, ProbabilisticScoringParameters};
use lightning::util::config::{ChannelHandshakeConfig, ChannelHandshakeLimits};
use lightning::util::ser::ReadableArgs;
use lightning_persister::FilesystemPersister;

use lampo_common::conf::{LampoConf, UserConfig};
use lampo_common::error;
use lampo_common::model::request;
use lampo_common::model::response;

use crate::chain::LampoChainManager;
use crate::ln::events::{ChangeStateChannelEvent, ChannelEvents};
use crate::persistence::LampoPersistence;
use crate::utils::logger::LampoLogger;

pub type LampoChainMonitor = ChainMonitor<
    InMemorySigner,
    Arc<dyn Filter + Send + Sync>,
    Arc<LampoChainManager>,
    Arc<LampoChainManager>,
    Arc<LampoLogger>,
    Arc<FilesystemPersister>,
>;

type LampoChanneld =
    SimpleArcChannelManager<LampoChainMonitor, LampoChainManager, LampoChainManager, LampoLogger>;

pub type LampoGraph = NetworkGraph<Arc<LampoLogger>>;
pub type LampoScorer = ProbabilisticScorer<Arc<LampoGraph>, Arc<LampoLogger>>;

pub struct LampoChannelManager {
    conf: LampoConf,
    monitor: Option<Arc<LampoChainMonitor>>,
    onchain: Arc<LampoChainManager>,
    persister: Arc<LampoPersistence>,
    graph: Option<Arc<LampoGraph>>,
    score: Option<Arc<Mutex<LampoScorer>>>,

    pub(crate) channeld: Option<Arc<LampoChanneld>>,
    pub(crate) logger: Arc<LampoLogger>,
}

impl LampoChannelManager {
    pub fn new(
        conf: &LampoConf,
        logger: Arc<LampoLogger>,
        onchain: Arc<LampoChainManager>,
        persister: Arc<LampoPersistence>,
    ) -> Self {
        LampoChannelManager {
            conf: conf.to_owned(),
            monitor: None,
            onchain,
            channeld: None,
            logger,
            persister,
            graph: None,
            score: None,
        }
    }

    fn build_channel_monitor(&self) -> LampoChainMonitor {
        ChainMonitor::new(
            Some(self.onchain.clone()),
            self.onchain.clone(),
            self.logger.clone(),
            self.onchain.clone(),
            self.persister.clone(),
        )
    }

    pub fn chain_monitor(&self) -> Arc<LampoChainMonitor> {
        let monitor = self.monitor.clone().unwrap();
        monitor.clone()
    }

    pub fn manager(&self) -> Arc<LampoChanneld> {
        let channeld = self.channeld.clone().unwrap();
        channeld
    }

    pub fn load_channel_monitors(&self, watch: bool) -> error::Result<()> {
        let keys = self.onchain.keymanager.inner();
        let mut monitors = self
            .persister
            .read_channelmonitors(keys.clone(), keys)
            .unwrap();
        if self.onchain.is_lightway() {
            for (_, chan_mon) in monitors.drain(..) {
                chan_mon.load_outputs_to_watch(&self.onchain);
                if watch {
                    let Some(monitor) = self.monitor.clone() else {
                        continue;
                    };

                    let outpoint = chan_mon.get_funding_txo().0;
                    monitor.watch_channel(outpoint, chan_mon);
                }
            }
        }
        Ok(())
    }

    pub fn graph(&self) -> Arc<LampoGraph> {
        let graph = self.graph.clone().unwrap();
        graph
    }

    pub fn scorer(&self) -> Arc<Mutex<LampoScorer>> {
        let score = self.score.clone().unwrap();
        score
    }

    // FIXME: Step 11: Optional: Initialize the NetGraphMsgHandler
    pub fn network_graph(
        &mut self,
    ) -> Arc<DefaultRouter<Arc<LampoGraph>, Arc<LampoLogger>, Arc<Mutex<LampoScorer>>>> {
        // Step 9: Initialize routing ProbabilisticScorer
        let network_graph_path = format!("{}/network_graph", self.conf.path());
        let network_graph = self.read_network(Path::new(&network_graph_path));

        let scorer_path = format!("{}/scorer", self.conf.path());
        let scorer = Arc::new(Mutex::new(
            self.read_scorer(Path::new(&scorer_path), &network_graph),
        ));

        self.graph = Some(network_graph.clone());
        self.score = Some(scorer.clone());
        Arc::new(DefaultRouter::new(
            network_graph.clone(),
            self.logger.clone(),
            self.onchain.keymanager.inner().get_secure_random_bytes(),
            scorer.clone(),
        ))
    }

    pub(crate) fn read_scorer(
        &self,
        path: &Path,
        graph: &Arc<LampoGraph>,
    ) -> ProbabilisticScorer<Arc<LampoGraph>, Arc<LampoLogger>> {
        let params = ProbabilisticScoringParameters::default();
        if let Ok(file) = File::open(path) {
            let args = (params.clone(), Arc::clone(&graph), self.logger.clone());
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

    pub fn restart(&self) {
        unimplemented!()
    }

    pub fn start(&mut self, block: BlockHash, height: Height) -> error::Result<()> {
        let chain_params = ChainParameters {
            network: self.conf.network,
            best_block: BestBlock::new(block, height.to_consensus_u32()),
        };

        let monitor = self.build_channel_monitor();
        let keymanagers = self.onchain.keymanager.inner();
        self.monitor = Some(Arc::new(monitor));
        self.channeld = Some(Arc::new(SimpleArcChannelManager::new(
            self.onchain.clone(),
            self.monitor.clone().unwrap().clone(),
            self.onchain.clone(),
            self.network_graph(),
            self.logger.clone(),
            keymanagers.clone(),
            keymanagers.clone(),
            keymanagers.clone(),
            self.conf.ldk_conf,
            chain_params,
        )));
        Ok(())
    }
}

impl ChannelEvents for LampoChannelManager {
    fn open_channel(
        &self,
        open_channel: request::OpenChannel,
    ) -> error::Result<response::OpenChannel> {
        let config = UserConfig {
            channel_handshake_limits: ChannelHandshakeLimits {
                // lnd's max to_self_delay is 2016, so we want to be compatible.
                their_to_self_delay: 2016,
                ..Default::default()
            },
            channel_handshake_config: ChannelHandshakeConfig {
                announced_channel: open_channel.public,
                ..Default::default()
            },
            ..Default::default()
        };
        self.manager()
            .create_channel(
                open_channel.node_id()?,
                open_channel.amount,
                0,
                0,
                Some(config),
                // FIXME: LDK should return a better error struct here
            )
            .map_err(|err| error::anyhow!("{:?}", err))?;
        Ok(response::OpenChannel {
            node_id: open_channel.node_id,
            amount: open_channel.amount,
            public: open_channel.public,
            push_mst: 0,
            to_self_delay: 2016,
        })
    }

    fn close_channel(&self) -> error::Result<()> {
        unimplemented!()
    }

    fn change_state_channel(&self, _: ChangeStateChannelEvent) -> error::Result<()> {
        unimplemented!()
    }
}
