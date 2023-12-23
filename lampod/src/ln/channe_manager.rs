//! Channel Manager Implementation
use std::cell::RefCell;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use lampo_common::bitcoin::hashes::hex::ToHex;
use lampo_common::bitcoin::locktime::Height;
use lampo_common::bitcoin::BlockHash;
use lampo_common::conf::{LampoConf, UserConfig};
use lampo_common::error;
use lampo_common::event::onchain::OnChainEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::keymanager::KeysManager;
use lampo_common::ldk::chain::chainmonitor::ChainMonitor;
use lampo_common::ldk::chain::channelmonitor::ChannelMonitor;
use lampo_common::ldk::chain::{BestBlock, Confirm, Filter, Watch};
use lampo_common::ldk::ln::channelmanager::{
    ChainParameters, ChannelManager, ChannelManagerReadArgs,
};
use lampo_common::ldk::persister::fs_store::FilesystemStore;
use lampo_common::ldk::routing::gossip::NetworkGraph;
use lampo_common::ldk::routing::router::DefaultRouter;
use lampo_common::ldk::routing::scoring::{
    ProbabilisticScorer, ProbabilisticScoringDecayParameters, ProbabilisticScoringFeeParameters,
};
use lampo_common::ldk::sign::EntropySource;
use lampo_common::ldk::sign::InMemorySigner;
use lampo_common::ldk::util::config::{ChannelHandshakeConfig, ChannelHandshakeLimits};
use lampo_common::ldk::util::persist::read_channel_monitors;
use lampo_common::ldk::util::ser::ReadableArgs;
use lampo_common::model::request;
use lampo_common::model::response::{self, Channel, Channels};

use crate::actions::handler::LampoHandler;
use crate::chain::{LampoChainManager, WalletManager};
use crate::ln::events::{ChangeStateChannelEvent, ChannelEvents};
use crate::persistence::LampoPersistence;
use crate::utils::logger::LampoLogger;

pub type LampoChainMonitor = ChainMonitor<
    InMemorySigner,
    Arc<dyn Filter + Send + Sync>,
    Arc<LampoChainManager>,
    Arc<LampoChainManager>,
    Arc<LampoLogger>,
    Arc<FilesystemStore>,
>;

pub type LampoArcChannelManager<M, T, F, L> = ChannelManager<
    Arc<M>,
    Arc<T>,
    Arc<KeysManager>,
    Arc<KeysManager>,
    Arc<KeysManager>,
    Arc<F>,
    Arc<
        DefaultRouter<
            Arc<NetworkGraph<Arc<L>>>,
            Arc<L>,
            Arc<Mutex<ProbabilisticScorer<Arc<NetworkGraph<Arc<L>>>, Arc<L>>>>,
            ProbabilisticScoringFeeParameters,
            ProbabilisticScorer<Arc<NetworkGraph<Arc<L>>>, Arc<L>>,
        >,
    >,
    Arc<L>,
>;

type LampoChannel =
    LampoArcChannelManager<LampoChainMonitor, LampoChainManager, LampoChainManager, LampoLogger>;

pub type LampoGraph = NetworkGraph<Arc<LampoLogger>>;
pub type LampoScorer = ProbabilisticScorer<Arc<LampoGraph>, Arc<LampoLogger>>;
pub type LampoRouter = DefaultRouter<
    Arc<LampoGraph>,
    Arc<LampoLogger>,
    Arc<Mutex<LampoScorer>>,
    ProbabilisticScoringFeeParameters,
    LampoScorer,
>;

pub struct LampoChannelManager {
    conf: LampoConf,
    monitor: Option<Arc<LampoChainMonitor>>,
    onchain: Arc<LampoChainManager>,
    wallet_manager: Arc<dyn WalletManager>,
    persister: Arc<LampoPersistence>,
    graph: Option<Arc<LampoGraph>>,
    score: Option<Arc<Mutex<LampoScorer>>>,
    handler: RefCell<Option<Arc<LampoHandler>>>,
    router: Option<Arc<LampoRouter>>,

    pub(crate) channeld: Option<Arc<LampoChannel>>,
    pub(crate) logger: Arc<LampoLogger>,
}

// SAFETY: due the init workflow of the lampod, we should
// store the handler later and not use the new contructor.
//
// Due the constructor is called only one time as the sethandler
// it is safe use the ref cell across thread.
unsafe impl Send for LampoChannelManager {}
unsafe impl Sync for LampoChannelManager {}

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
            monitor: None,
            onchain,
            channeld: None,
            wallet_manager,
            logger,
            persister,
            handler: RefCell::new(None),
            graph: None,
            score: None,
            router: None,
        }
    }

    pub fn set_handler(&self, handler: Arc<LampoHandler>) {
        self.handler.replace(Some(handler));
    }

    pub fn handler(&self) -> Arc<LampoHandler> {
        self.handler.borrow().clone().unwrap()
    }

    pub fn listen(self: Arc<Self>) -> JoinHandle<()> {
        if self.is_restarting().unwrap() {
            self.resume_channels().unwrap();
            self.load_channel_monitors(true).unwrap();
        }
        std::thread::spawn(move || {
            log::info!(target: "lampo_channel_manager", "listening on chain event on the channel manager");
            let events = self.handler().events();
            loop {
                let Ok(Event::OnChain(event)) = events.recv() else {
                    continue;
                };
                log::trace!(target: "lampo_channel_manager", "event received {:?}", event);
                match event {
                    OnChainEvent::NewBestBlock((hash, height)) => {
                        log::debug!(target: "lampo_channel_manager", "new best block with hash `{}` at height `{height}`", hash.block_hash());
                        self.chain_monitor()
                            .best_block_updated(&hash, height.to_consensus_u32());
                        self.manager()
                            .best_block_updated(&hash, height.to_consensus_u32());
                    }
                    OnChainEvent::ConfirmedTransaction((tx, idx, header, height)) => {
                        log::debug!(target: "lampo_channel_manager", "confirmed transaction with txid `{}` at height `{height}`", tx.txid());
                        self.chain_monitor().transactions_confirmed(
                            &header,
                            &[(idx as usize, &tx)],
                            height.to_consensus_u32(),
                        );
                        self.manager().transactions_confirmed(
                            &header,
                            &[(idx as usize, &tx)],
                            height.to_consensus_u32(),
                        );
                    }
                    _ => continue,
                }
            }
        })
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
        self.monitor.clone().unwrap()
    }

    pub fn manager(&self) -> Arc<LampoChannel> {
        self.channeld.clone().unwrap()
    }

    pub fn list_channel(&self) -> Channels {
        let channels: Vec<Channel> = self
            .manager()
            .list_channels()
            .into_iter()
            .map(|channel| Channel {
                short_channel_id: channel.short_channel_id,
                peer_id: channel.counterparty.node_id.to_hex(),
                peer_alias: None,
                ready: channel.is_channel_ready,
                amount_satoshis: channel.channel_value_satoshis,
                amount_msat: channel.next_outbound_htlc_limit_msat,
                public: channel.is_public,
                available_balance_for_send_msat: channel.outbound_capacity_msat,
                available_balance_for_recv_msat: channel.inbound_capacity_msat,
            })
            .collect();
        Channels { channels }
    }

    pub fn load_channel_monitors(&self, watch: bool) -> error::Result<()> {
        let keys = self.wallet_manager.ldk_keys().inner();
        let mut monitors = read_channel_monitors(self.persister.clone(), keys.clone(), keys)?;
        for (_, chan_mon) in monitors.drain(..) {
            chan_mon.load_outputs_to_watch(&self.onchain);
            if watch {
                let monitor = self
                    .monitor
                    .clone()
                    .ok_or(error::anyhow!("Channel Monitor not present"))?;
                let outpoint = chan_mon.get_funding_txo().0;
                monitor
                    .watch_channel(outpoint, chan_mon)
                    .map_err(|err| error::anyhow!("{:?}", err))?;
            }
        }
        Ok(())
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
        self.graph.clone().unwrap()
    }

    pub fn scorer(&self) -> Arc<Mutex<LampoScorer>> {
        self.score.clone().unwrap()
    }

    // FIXME: Step 11: Optional: Initialize the NetGraphMsgHandler
    pub fn network_graph(
        &mut self,
    ) -> Arc<
        DefaultRouter<
            Arc<LampoGraph>,
            Arc<LampoLogger>,
            Arc<Mutex<LampoScorer>>,
            ProbabilisticScoringFeeParameters,
            LampoScorer,
        >,
    > {
        if self.router.is_none() {
            // Step 9: Initialize routing ProbabilisticScorer
            let network_graph_path = format!("{}/network_graph", self.conf.path());
            let network_graph = self.read_network(Path::new(&network_graph_path));

            let scorer_path = format!("{}/scorer", self.conf.path());
            let scorer = Arc::new(Mutex::new(
                self.read_scorer(Path::new(&scorer_path), &network_graph),
            ));

            self.graph = Some(network_graph.clone());
            self.score = Some(scorer.clone());
            self.router = Some(Arc::new(DefaultRouter::new(
                network_graph,
                self.logger.clone(),
                self.wallet_manager
                    .ldk_keys()
                    .keys_manager
                    .get_secure_random_bytes(),
                scorer,
                ProbabilisticScoringFeeParameters::default(),
            )))
        }
        self.router.clone().unwrap()
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

    pub fn is_restarting(&self) -> error::Result<bool> {
        Ok(Path::exists(Path::new(&format!(
            "{}/manager",
            self.conf.path()
        ))))
    }

    pub fn restart(&mut self) -> error::Result<()> {
        let monitor = self.build_channel_monitor();
        self.monitor = Some(Arc::new(monitor));
        let _ = self.network_graph();
        let mut monitors = self.get_channel_monitors()?;
        let monitors = monitors.iter_mut().collect::<Vec<_>>();
        let read_args = ChannelManagerReadArgs::new(
            self.wallet_manager.ldk_keys().keys_manager.clone(),
            self.wallet_manager.ldk_keys().keys_manager.clone(),
            self.wallet_manager.ldk_keys().keys_manager.clone(),
            self.onchain.clone(),
            self.chain_monitor(),
            self.onchain.clone(),
            self.router.clone().unwrap(),
            self.logger.clone(),
            self.conf.ldk_conf,
            monitors,
        );
        let mut channel_manager_file = File::open(format!("{}/manager", self.conf.path()))?;
        let (_, channel_manager) =
            <(BlockHash, LampoChannel)>::read(&mut channel_manager_file, read_args)
                .map_err(|err| error::anyhow!("{err}"))?;
        self.channeld = Some(channel_manager.into());
        Ok(())
    }

    pub fn resume_channels(&self) -> error::Result<()> {
        let mut relevant_txids_one = self
            .channeld
            .clone()
            .unwrap()
            .get_relevant_txids()
            .iter()
            .map(|(txid, _)| txid.clone())
            .collect::<Vec<_>>();
        let mut relevant_txids_two = self
            .chain_monitor()
            .get_relevant_txids()
            .iter()
            .map(|(txid, _)| txid.clone())
            .collect::<Vec<_>>();
        log::debug!(
            "transactions {:?} {:?}",
            relevant_txids_one,
            relevant_txids_two
        );
        // FIXME: check if some of these transaction are out of chain
        self.onchain
            .backend
            .manage_transactions(&mut relevant_txids_one)?;
        self.onchain
            .backend
            .manage_transactions(&mut relevant_txids_two)?;
        self.onchain.backend.process_transactions()?;
        Ok(())
    }

    pub fn start(
        &mut self,
        block: BlockHash,
        height: Height,
        block_timestamp: u32,
    ) -> error::Result<()> {
        let chain_params = ChainParameters {
            network: self.conf.network,
            best_block: BestBlock::new(block, height.to_consensus_u32()),
        };

        let monitor = self.build_channel_monitor();
        self.monitor = Some(Arc::new(monitor));

        let keymanagers = self.wallet_manager.ldk_keys().keys_manager.clone();
        self.channeld = Some(Arc::new(LampoArcChannelManager::new(
            self.onchain.clone(),
            self.monitor.clone().unwrap(),
            self.onchain.clone(),
            self.network_graph(),
            self.logger.clone(),
            keymanagers.clone(),
            keymanagers.clone(),
            keymanagers,
            self.conf.ldk_conf,
            chain_params,
            block_timestamp,
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
            tx: None,
        })
    }

    fn close_channel(&self) -> error::Result<()> {
        unimplemented!()
    }

    fn change_state_channel(&self, _: ChangeStateChannelEvent) -> error::Result<()> {
        unimplemented!()
    }
}
