//! Lampo Route implementation

use std::ops::Deref;

use lampo_common::error;
use lampo_common::ldk::blinded_path::payment::ForwardNode;
use lampo_common::ldk::blinded_path::payment::ForwardTlvs;
use lampo_common::ldk::blinded_path::payment::PaymentConstraints;
use lampo_common::ldk::blinded_path::payment::PaymentRelay;
use lampo_common::ldk::blinded_path::payment::ReceiveTlvs;
use lampo_common::ldk::blinded_path::BlindedPath;
use lampo_common::ldk::ln::channelmanager::ChannelDetails;
use lampo_common::ldk::ln::channelmanager::MIN_FINAL_CLTV_EXPIRY_DELTA;
use lampo_common::ldk::ln::features::BlindedHopFeatures;
use lampo_common::ldk::ln::msgs::LightningError;
use lampo_common::ldk::offers::invoice::BlindedPayInfo;
use lampo_common::ldk::onion_message::messenger::MessageRouter;
use lampo_common::ldk::routing::gossip::NodeId;
use lampo_common::ldk::routing::router::find_route;
use lampo_common::ldk::routing::router::InFlightHtlcs;
use lampo_common::ldk::routing::router::Route;
use lampo_common::ldk::routing::router::RouteParameters;
use lampo_common::ldk::routing::router::Router;
use lampo_common::ldk::routing::router::ScorerAccountingForInFlightHtlcs;
use lampo_common::secp256k1;

use crate::ln::route::secp256k1::PublicKey;
use crate::ln::route::secp256k1::Secp256k1;

use lampo_common::ldk::{
    routing::{
        gossip::NetworkGraph,
        scoring::{LockableScore, ScoreLookUp},
    },
    sign::EntropySource,
    util::logger::Logger,
};

use super::onion_message::LampoMsgRouter;

/// A [`Router`] implemented using [`find_route`].
pub struct LampoRouter<
    G: Deref<Target = NetworkGraph<L>> + Clone,
    L: Deref,
    ES: Deref,
    S: Deref,
    SP: Sized,
    Sc: ScoreLookUp<ScoreParams = SP>,
> where
    L::Target: Logger,
    S::Target: for<'a> LockableScore<'a, ScoreLookUp = Sc>,
    ES::Target: EntropySource,
{
    network_graph: G,
    logger: L,
    entropy_source: ES,
    scorer: S,
    score_params: SP,
    message_router: LampoMsgRouter<G, L, ES>,
}

impl<
        G: Deref<Target = NetworkGraph<L>> + Clone,
        L: Deref,
        ES: Deref + Clone,
        S: Deref,
        SP: Sized,
        Sc: ScoreLookUp<ScoreParams = SP>,
    > LampoRouter<G, L, ES, S, SP, Sc>
where
    L::Target: Logger,
    S::Target: for<'a> LockableScore<'a, ScoreLookUp = Sc>,
    ES::Target: EntropySource,
{
    /// Creates a new router.
    pub fn new(
        network_graph: G,
        logger: L,
        entropy_source: ES,
        scorer: S,
        score_params: SP,
    ) -> error::Result<Self> {
        let message_router =
            LampoMsgRouter::<G, L, ES>::new(network_graph.clone(), entropy_source.clone())?;
        Ok(Self {
            network_graph,
            logger,
            entropy_source,
            scorer,
            score_params,
            message_router,
        })
    }
}

impl<
        G: Deref<Target = NetworkGraph<L>> + Clone,
        L: Deref,
        ES: Deref + Clone,
        S: Deref,
        SP: Sized,
        Sc: ScoreLookUp<ScoreParams = SP>,
    > Router for LampoRouter<G, L, ES, S, SP, Sc>
where
    L::Target: Logger,
    S::Target: for<'a> LockableScore<'a, ScoreLookUp = Sc>,
    ES::Target: EntropySource,
{
    fn find_route(
        &self,
        payer: &PublicKey,
        params: &RouteParameters,
        first_hops: Option<&[&ChannelDetails]>,
        inflight_htlcs: InFlightHtlcs,
    ) -> Result<Route, LightningError> {
        let random_seed_bytes = self.entropy_source.get_secure_random_bytes();
        find_route(
            payer,
            params,
            &self.network_graph,
            first_hops,
            &*self.logger,
            &ScorerAccountingForInFlightHtlcs::new(self.scorer.read_lock(), &inflight_htlcs),
            &self.score_params,
            &random_seed_bytes,
        )
    }

    fn create_blinded_payment_paths<T: secp256k1::Signing + secp256k1::Verification>(
        &self,
        recipient: PublicKey,
        first_hops: Vec<ChannelDetails>,
        tlvs: ReceiveTlvs,
        amount_msats: u64,
        secp_ctx: &Secp256k1<T>,
    ) -> Result<Vec<(BlindedPayInfo, BlindedPath)>, ()> {
        // Limit the number of blinded paths that are computed.
        const MAX_PAYMENT_PATHS: usize = 3;

        // Ensure peers have at least three channels so that it is more difficult to infer the
        // recipient's node_id.
        const MIN_PEER_CHANNELS: usize = 3;

        let network_graph = self.network_graph.deref().read_only();
        let paths = first_hops
            .into_iter()
            .filter(|details| details.counterparty.features.supports_route_blinding())
            .filter(|details| amount_msats <= details.inbound_capacity_msat)
            .filter(|details| amount_msats >= details.inbound_htlc_minimum_msat.unwrap_or(0))
            .filter(|details| amount_msats <= details.inbound_htlc_maximum_msat.unwrap_or(u64::MAX))
            .filter(|details| {
                network_graph
                    .node(&NodeId::from_pubkey(&details.counterparty.node_id))
                    .map(|node_info| node_info.channels.len() >= MIN_PEER_CHANNELS)
                    .unwrap_or(false)
            })
            .filter_map(|details| {
                let short_channel_id = match details.get_inbound_payment_scid() {
                    Some(short_channel_id) => short_channel_id,
                    None => return None,
                };
                let payment_relay: PaymentRelay = match details.counterparty.forwarding_info {
                    Some(forwarding_info) => match forwarding_info.try_into() {
                        Ok(payment_relay) => payment_relay,
                        Err(()) => return None,
                    },
                    None => return None,
                };

                let cltv_expiry_delta = payment_relay.cltv_expiry_delta as u32;
                let payment_constraints = PaymentConstraints {
                    max_cltv_expiry: tlvs.payment_constraints.max_cltv_expiry + cltv_expiry_delta,
                    htlc_minimum_msat: details.inbound_htlc_minimum_msat.unwrap_or(0),
                };
                Some(ForwardNode {
                    tlvs: ForwardTlvs {
                        short_channel_id,
                        payment_relay,
                        payment_constraints,
                        features: BlindedHopFeatures::empty(),
                    },
                    node_id: details.counterparty.node_id,
                    htlc_maximum_msat: details.inbound_htlc_maximum_msat.unwrap_or(u64::MAX),
                })
            })
            .map(|forward_node| {
                BlindedPath::new_for_payment(
                    &[forward_node],
                    recipient,
                    tlvs.clone(),
                    u64::MAX,
                    MIN_FINAL_CLTV_EXPIRY_DELTA,
                    &*self.entropy_source,
                    secp_ctx,
                )
            })
            .take(MAX_PAYMENT_PATHS)
            .collect::<Result<Vec<_>, _>>();

        match paths {
            Ok(paths) if !paths.is_empty() => Ok(paths),
            _ => {
                if network_graph
                    .nodes()
                    .contains_key(&NodeId::from_pubkey(&recipient))
                {
                    BlindedPath::one_hop_for_payment(
                        recipient,
                        tlvs,
                        MIN_FINAL_CLTV_EXPIRY_DELTA,
                        &*self.entropy_source,
                        secp_ctx,
                    )
                    .map(|path| vec![path])
                } else {
                    Err(())
                }
            }
        }
    }
}

impl<
        G: Deref<Target = NetworkGraph<L>> + Clone,
        L: Deref,
        ES: Deref + Clone,
        S: Deref,
        SP: Sized,
        Sc: ScoreLookUp<ScoreParams = SP>,
    > MessageRouter for LampoRouter<G, L, ES, S, SP, Sc>
where
    L::Target: Logger,
    S::Target: for<'a> LockableScore<'a, ScoreLookUp = Sc>,
    ES::Target: EntropySource,
{
    fn create_blinded_paths<
        T: lampo_common::secp256k1::Signing + lampo_common::secp256k1::Verification,
    >(
        &self,
        recipient: lampo_common::secp256k1::PublicKey,
        peers: Vec<lampo_common::secp256k1::PublicKey>,
        secp_ctx: &lampo_common::secp256k1::Secp256k1<T>,
    ) -> Result<Vec<lampo_common::ldk::blinded_path::BlindedPath>, ()> {
        self.message_router
            .create_blinded_paths(recipient, peers, secp_ctx)
    }

    fn find_path(
        &self,
        sender: lampo_common::secp256k1::PublicKey,
        peers: Vec<lampo_common::secp256k1::PublicKey>,
        destination: lampo_common::ldk::onion_message::messenger::Destination,
    ) -> Result<lampo_common::ldk::onion_message::messenger::OnionMessagePath, ()> {
        self.message_router.find_path(sender, peers, destination)
    }
}
