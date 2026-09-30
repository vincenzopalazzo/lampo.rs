//! Lampo Common Types
use std::sync::{Arc, Mutex};

use lightning::chain::chaininterface::{BroadcasterInterface, FeeEstimator};
use lightning::onion_message::messenger::DefaultMessageRouter;

use crate::bitcoin::secp256k1::PublicKey;
use crate::ldk::chain::chainmonitor::ChainMonitor;
use crate::ldk::chain::Filter;
use crate::ldk::ln::channelmanager::ChannelManager;
use crate::ldk::persister::fs_store::v1::FilesystemStore;
use crate::ldk::routing::gossip::NetworkGraph;
use crate::ldk::routing::router::DefaultRouter;
use crate::ldk::routing::scoring::{ProbabilisticScorer, ProbabilisticScoringFeeParameters};
use crate::ldk::util::sweep::OutputSweeper;
use crate::signer::{LampoChangeDestination, LampoChannelSigner, LampoSigner};
use crate::utils::logger::LampoLogger;

pub type NodeId = PublicKey;
pub type ChannelId = crate::ldk::ln::types::ChannelId;

pub type LampoChainMonitor = ChainMonitor<
    LampoChannelSigner,
    Arc<dyn Filter + Send + Sync>,
    Arc<dyn BroadcasterInterface + Send + Sync>,
    Arc<dyn FeeEstimator + Send + Sync>,
    Arc<LampoLogger>,
    Arc<FilesystemStore>,
    Arc<dyn LampoSigner>,
>;

pub type LampoArcChannelManager<M, L> = ChannelManager<
    Arc<M>,
    Arc<dyn BroadcasterInterface + Send + Sync>,
    Arc<dyn LampoSigner>,
    Arc<dyn LampoSigner>,
    Arc<dyn LampoSigner>,
    Arc<dyn FeeEstimator + Send + Sync>,
    Arc<LampoRouter>,
    Arc<DefaultMessageRouter<Arc<LampoGraph>, Arc<LampoLogger>, Arc<dyn LampoSigner>>>,
    Arc<L>,
>;

pub type LampoChannel = LampoArcChannelManager<LampoChainMonitor, LampoLogger>;

/// Tracks and sweeps spendable outputs of closed channels back into the
/// on-chain wallet. Fed by `Event::SpendableOutputs`, driven by the
/// background processor, and notified of blocks by the chain backend.
pub type LampoSweeper = OutputSweeper<
    Arc<dyn BroadcasterInterface + Send + Sync>,
    Arc<LampoChangeDestination>,
    Arc<dyn FeeEstimator + Send + Sync>,
    Arc<dyn Filter + Send + Sync>,
    Arc<FilesystemStore>,
    Arc<LampoLogger>,
    Arc<dyn LampoSigner>,
>;

pub type LampoGraph = NetworkGraph<Arc<LampoLogger>>;
pub type LampoScorer = ProbabilisticScorer<Arc<LampoGraph>, Arc<LampoLogger>>;
pub type LampoRouter = DefaultRouter<
    Arc<LampoGraph>,
    Arc<LampoLogger>,
    Arc<dyn LampoSigner>,
    Arc<Mutex<LampoScorer>>,
    ProbabilisticScoringFeeParameters,
    LampoScorer,
>;
