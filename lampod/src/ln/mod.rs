//! Lampo Channel Manager
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::{Arc, Mutex};

use bitcoin::locktime::Height;
use bitcoin::BlockHash;
use lightning::chain::chainmonitor::ChainMonitor;
use lightning::chain::keysinterface::EntropySource;
use lightning::chain::keysinterface::InMemorySigner;
use lightning::chain::{BestBlock, Filter};
use lightning::ln::channelmanager::{ChainParameters, SimpleArcChannelManager};
use lightning::routing::gossip::NetworkGraph;
use lightning::routing::router::DefaultRouter;
use lightning::routing::scoring::{ProbabilisticScorer, ProbabilisticScoringParameters};
use lightning::util::ser::ReadableArgs;
use lightning_persister::FilesystemPersister;

use crate::chain::LampoChainManager;
use crate::conf::LampoConf;
use crate::persistence::LampoPersistence;
use crate::utils::logger::LampoLogger;

type LampoChannelMonitor = ChainMonitor<
    InMemorySigner,
    Arc<dyn Filter + Send + Sync>,
    Arc<LampoChainManager>,
    Arc<LampoChainManager>,
    Arc<LampoLogger>,
    Arc<FilesystemPersister>,
>;

type LampoChanneld =
    SimpleArcChannelManager<LampoChannelMonitor, LampoChainManager, LampoChainManager, LampoLogger>;

type LampoGraph = NetworkGraph<Arc<LampoLogger>>;
type LampoScorer = ProbabilisticScorer<Arc<LampoGraph>, Arc<LampoLogger>>;

pub struct LampoChannelManager {
    conf: LampoConf,
    monitors: Option<LampoChannelMonitor>,
    onchain: Arc<LampoChainManager>,
    channeld: Option<LampoChanneld>,
    logger: Arc<LampoLogger>,
    persister: Arc<LampoPersistence>,
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
            monitors: None,
            onchain,
            channeld: None,
            logger,
            persister,
        }
    }

    fn build_channel_monitor(
        &self,
        logger: Arc<LampoLogger>,
        persister: &Arc<LampoPersistence>,
    ) -> LampoChannelMonitor {
        ChainMonitor::new(
            Some(self.onchain.clone()),
            self.onchain.clone(),
            logger,
            self.onchain.clone(),
            persister.clone(),
        )
    }

    pub fn network_graph(
        &self,
    ) -> Arc<DefaultRouter<Arc<LampoGraph>, Arc<LampoLogger>, Arc<Mutex<LampoScorer>>>> {
        // Step 9: Initialize routing ProbabilisticScorer
        let network_graph_path = format!("{}/network_graph", self.conf.path);
        let network_graph = self.read_network(Path::new(&network_graph_path));

        let scorer_path = format!("{}/scorer", self.conf.path);
        let scorer = Arc::new(Mutex::new(
            self.read_scorer(Path::new(&scorer_path), &network_graph),
        ));

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

    pub async fn start(&mut self, block: BlockHash, height: Height) -> Result<(), String> {
        let chain_params = ChainParameters {
            network: self.conf.network, // substitute this with your network
            best_block: BestBlock::new(block, height.to_consensus_u32()),
        };

        let monitor = self.build_channel_monitor(Arc::clone(&self.logger), &self.persister);
        let keymanagers = self.onchain.keymanager.inner();
        self.channeld = Some(SimpleArcChannelManager::new(
            self.onchain.clone(),
            Arc::new(monitor),
            self.onchain.clone(),
            self.network_graph(),
            self.logger.clone(),
            keymanagers.clone(),
            keymanagers.clone(),
            keymanagers.clone(),
            self.conf.ldk_conf,
            chain_params,
        ));
        Ok(())
    }
}
