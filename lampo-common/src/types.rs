//! Lampo Common Types
use std::sync::{Arc, Mutex};

use lightning::chain::chaininterface::{BroadcasterInterface, FeeEstimator};
use lightning::onion_message::messenger::DefaultMessageRouter;

use crate::bitcoin::secp256k1::PublicKey;
use crate::ldk::chain::chainmonitor::ChainMonitor;
use crate::ldk::chain::channelmonitor::ChannelMonitor;
use crate::ldk::chain::Filter;
use crate::ldk::ln::channelmanager::ChannelManager;
use crate::ldk::persister::fs_store::v1::FilesystemStore;
use crate::ldk::routing::gossip::NetworkGraph;
use crate::ldk::routing::router::DefaultRouter;
use crate::ldk::routing::scoring::{ProbabilisticScorer, ProbabilisticScoringFeeParameters};
use crate::ldk::sign::InMemorySigner;

use crate::keys::LampoKeysManager;
use crate::utils::logger::LampoLogger;

pub type NodeId = PublicKey;
pub type ChannelId = crate::ldk::ln::types::ChannelId;

pub type LampoChainMonitor = ChainMonitor<
    InMemorySigner,
    Arc<dyn Filter + Send + Sync>,
    Arc<dyn BroadcasterInterface + Send + Sync>,
    Arc<dyn FeeEstimator + Send + Sync>,
    Arc<LampoLogger>,
    Arc<FilesystemStore>,
    Arc<LampoKeysManager>,
>;

pub type LampoArcChannelManager<M, L> = ChannelManager<
    Arc<M>,
    Arc<dyn BroadcasterInterface + Send + Sync>,
    Arc<LampoKeysManager>,
    Arc<LampoKeysManager>,
    Arc<LampoKeysManager>,
    Arc<dyn FeeEstimator + Send + Sync>,
    Arc<LampoRouter>,
    Arc<DefaultMessageRouter<Arc<LampoGraph>, Arc<LampoLogger>, Arc<LampoKeysManager>>>,
    Arc<L>,
>;

pub type LampoChannel = LampoArcChannelManager<LampoChainMonitor, LampoLogger>;

/// A [`ChannelMonitor`] read from disk on restart, paired with the chain
/// context it needs to replay blocks on its own (LDK implements
/// [`chain::Listen`] for this tuple). Each monitor must be synced from its
/// own best block up to the chain tip and then registered with the
/// [`LampoChainMonitor`] via `watch_channel` before the node goes live.
///
/// [`chain::Listen`]: crate::ldk::chain::Listen
pub type LampoMonitorListener = (
    ChannelMonitor<InMemorySigner>,
    Arc<dyn BroadcasterInterface + Send + Sync>,
    Arc<dyn FeeEstimator + Send + Sync>,
    Arc<LampoLogger>,
);

pub type LampoGraph = NetworkGraph<Arc<LampoLogger>>;
pub type LampoScorer = ProbabilisticScorer<Arc<LampoGraph>, Arc<LampoLogger>>;
pub type LampoRouter = DefaultRouter<
    Arc<LampoGraph>,
    Arc<LampoLogger>,
    Arc<LampoKeysManager>,
    Arc<Mutex<LampoScorer>>,
    ProbabilisticScoringFeeParameters,
    LampoScorer,
>;
