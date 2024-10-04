//! Channel Manager Implementation
use std::cell::RefCell;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use lampo_common::bitcoin::{BlockHash, Transaction};
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::event::onchain::OnChainEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::keys::LampoKeysManager;
use lampo_common::ldk::block_sync::BlockSource;
use lampo_common::ldk::chain::chaininterface::{BroadcasterInterface, FeeEstimator};
use lampo_common::ldk::chain::chainmonitor::ChainMonitor;
use lampo_common::ldk::chain::channelmonitor::ChannelMonitor;
use lampo_common::ldk::chain::BestBlock;
use lampo_common::ldk::ln::channelmanager::{ChainParameters, ChannelManagerReadArgs};
use lampo_common::ldk::routing::gossip::NetworkGraph;
use lampo_common::ldk::routing::router::DefaultRouter;
use lampo_common::ldk::routing::scoring::{
    ProbabilisticScorer, ProbabilisticScoringDecayParameters, ProbabilisticScoringFeeParameters,
};
use lampo_common::ldk::sign::InMemorySigner;
use lampo_common::ldk::util::persist::read_channel_monitors;
use lampo_common::ldk::util::ser::ReadableArgs;
use lampo_common::model::request;
use lampo_common::model::response::{self, Channel, Channels};
use lampo_common::types::LampoChannel;
use lampo_common::types::LampoGraph;
use lampo_common::types::LampoRouter;
use lampo_common::types::LampoScorer;
use lampo_common::types::{LampoArcChannelManager, LampoChainMonitor};

use crate::actions::handler::LampoHandler;
use crate::async_run;
use crate::chain::{LampoChainManager, WalletManager};
use crate::persistence::LampoPersistence;
use crate::utils::logger::LampoLogger;

pub struct LampoChannelManager {
    monitor: RefCell<Option<Arc<LampoChainMonitor>>>,
    wallet_manager: Arc<dyn WalletManager>,
    persister: Arc<LampoPersistence>,
    graph: RefCell<Option<Arc<LampoGraph>>>,
    score: RefCell<Option<Arc<Mutex<LampoScorer>>>>,
    handler: RefCell<Option<Arc<LampoHandler>>>,
    router: RefCell<Option<Arc<LampoRouter>>>,

    pub(crate) onchain: Arc<LampoChainManager>,
    pub(crate) conf: LampoConf,
    pub(crate) channeld: RefCell<Option<Arc<LampoChannel>>>,
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
            monitor: RefCell::new(None),
            onchain,
            channeld: RefCell::new(None),
            wallet_manager,
            logger,
            persister,
            handler: RefCell::new(None),
            graph: RefCell::new(None),
            score: RefCell::new(None),
            router: RefCell::new(None),
        }
    }

    pub fn set_handler(&self, handler: Arc<LampoHandler>) {
        self.handler.replace(Some(handler));
    }

    pub fn handler(&self) -> Arc<LampoHandler> {
        self.handler.borrow().clone().unwrap()
    }

    pub fn listen(self: Arc<Self>) -> error::Result<()> {
        if self.is_restarting()? {
            self.restart()?;
        } else {
            self.start()?;
        }
        Ok(())
    }

    fn build_channel_monitor(&self) -> LampoChainMonitor {
        ChainMonitor::new(
            // FIXME: this is needed when use esplora or electrum
            None,
            self.onchain.clone(),
            self.logger.clone(),
            self.onchain.clone(),
            self.persister.clone(),
        )
    }

    pub fn chain_monitor(&self) -> Arc<LampoChainMonitor> {
        self.monitor.borrow().clone().unwrap()
    }

    pub fn manager(&self) -> Arc<LampoChannel> {
        self.channeld.borrow().clone().unwrap()
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
                public: channel.is_public,
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
        self.graph.borrow().clone().unwrap()
    }

    pub fn scorer(&self) -> Arc<Mutex<LampoScorer>> {
        self.score.borrow().clone().unwrap()
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
        let router = self.router.borrow().clone();
        if router.is_none() {
            // Step 9: Initialize routing ProbabilisticScorer
            let network_graph_path = format!("{}/network_graph", self.conf.path());
            let network_graph = self.read_network(Path::new(&network_graph_path));

            let scorer_path = format!("{}/scorer", self.conf.path());
            let scorer = Arc::new(Mutex::new(
                self.read_scorer(Path::new(&scorer_path), &network_graph),
            ));

            self.graph.replace(Some(network_graph.clone()));
            self.score.replace(Some(scorer.clone()));
            let router = Some(Arc::new(DefaultRouter::new(
                network_graph,
                self.logger.clone(),
                self.wallet_manager.ldk_keys().keys_manager.clone(),
                scorer,
                ProbabilisticScoringFeeParameters::default(),
            )));
            self.router.replace(router);
        }
        self.router.borrow().clone().unwrap()
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

    pub fn open_channel(
        &self,
        open_channel: request::OpenChannel,
    ) -> error::Result<response::OpenChannel> {
        self.manager()
            .create_channel(
                open_channel.node_id()?,
                open_channel.amount,
                0,
                0,
                None,
                Some(self.conf.ldk_conf),
            )
            .map_err(|err| error::anyhow!("{:?}", err))?;

        // Wait for SendRawTransaction to be received so to get the funding transaction
        // FIXME: we can loop forever here
        let tx: Option<Transaction> = loop {
            let events = self.handler().events();
            let event = events.recv_timeout(std::time::Duration::from_secs(30))?;

            if let Event::OnChain(OnChainEvent::SendRawTransaction(tx)) = event {
                break Some(tx);
            }
        };

        let txid = tx.as_ref().map(|tx| tx.txid());

        Ok(response::OpenChannel {
            node_id: open_channel.node_id,
            amount: open_channel.amount,
            public: open_channel.public,
            push_msat: 0,
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

    pub fn is_restarting(&self) -> error::Result<bool> {
        Ok(Path::exists(Path::new(&format!(
            "{}/manager",
            self.conf.path()
        ))))
    }

    pub fn restart(&self) -> error::Result<()> {
        let monitor = self.build_channel_monitor();
        self.monitor.replace(Some(Arc::new(monitor)));

        let _ = self.network_graph();
        let mut monitors = self.get_channel_monitors()?;
        let monitors = monitors.iter_mut().collect::<Vec<_>>();
        let read_args = ChannelManagerReadArgs::new(
            self.wallet_manager.ldk_keys().keys_manager.clone(),
            self.wallet_manager.ldk_keys().keys_manager.clone(),
            self.wallet_manager.ldk_keys().keys_manager.clone(),
            self.onchain.clone() as Arc<dyn FeeEstimator + Send + Sync>,
            self.chain_monitor(),
            self.onchain.clone() as Arc<dyn BroadcasterInterface + Send + Sync>,
            self.router.borrow().clone().unwrap(),
            self.logger.clone(),
            self.conf.ldk_conf,
            monitors,
        );
        let mut channel_manager_file = File::open(format!("{}/manager", self.conf.path()))?;
        let (_, channel_manager) =
            <(BlockHash, LampoChannel)>::read(&mut channel_manager_file, read_args)
                .map_err(|err| error::anyhow!("{err}"))?;
        self.channeld.borrow_mut().replace(channel_manager.into());
        Ok(())
    }

    pub fn start(&self) -> error::Result<()> {
        let (block_hash, block_height) = async_run!(self.onchain.get_best_block()).unwrap();
        let chain_params = ChainParameters {
            network: self.conf.network,
            best_block: BestBlock {
                block_hash: block_hash,
                // FIXME: the default could be dangerus here
                height: block_height.unwrap_or_default(),
            },
        };

        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?;
        let monitor = self.build_channel_monitor();
        self.monitor.replace(Some(Arc::new(monitor)));

        let keymanagers = self.wallet_manager.ldk_keys().keys_manager.clone();
        let channeld = Arc::new(LampoArcChannelManager::new(
            self.onchain.clone(),
            self.chain_monitor(),
            self.onchain.clone(),
            self.network_graph(),
            self.logger.clone(),
            keymanagers.clone(),
            keymanagers.clone(),
            keymanagers,
            self.conf.ldk_conf,
            chain_params,
            now.as_secs() as u32,
        ));
        self.channeld.borrow_mut().replace(channeld);
        Ok(())
    }
}
