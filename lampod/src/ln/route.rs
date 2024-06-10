//! Lampo Route implementation

use std::ops::Deref;

#[cfg(feature = "vanilla")]
pub use {
    crate::ln::route::secp256k1::Secp256k1,
    lampo_common::error,
    lampo_common::ldk::blinded_path::payment::ForwardNode,
    lampo_common::ldk::blinded_path::payment::ForwardTlvs,
    lampo_common::ldk::blinded_path::payment::PaymentConstraints,
    lampo_common::ldk::blinded_path::payment::PaymentRelay,
    lampo_common::ldk::blinded_path::payment::ReceiveTlvs,
    lampo_common::ldk::blinded_path::BlindedPath,
    lampo_common::ldk::ln::channelmanager::ChannelDetails,
    lampo_common::ldk::ln::channelmanager::MIN_FINAL_CLTV_EXPIRY_DELTA,
    lampo_common::ldk::ln::features::BlindedHopFeatures,
    lampo_common::ldk::ln::msgs::LightningError,
    lampo_common::ldk::offers::invoice::BlindedPayInfo,
    lampo_common::ldk::onion_message::messenger,
    lampo_common::ldk::onion_message::messenger::MessageRouter,
    lampo_common::ldk::routing::gossip::NodeId,
    lampo_common::ldk::routing::router::find_route,
    lampo_common::ldk::routing::router::InFlightHtlcs,
    lampo_common::ldk::routing::router::Route,
    lampo_common::ldk::routing::router::RouteParameters,
    lampo_common::ldk::routing::router::Router,
    lampo_common::ldk::routing::router::ScorerAccountingForInFlightHtlcs,
    lampo_common::ldk::{
        routing::{
            gossip::NetworkGraph,
            scoring::{LockableScore, ScoreLookUp},
        },
        sign::EntropySource,
        util::logger::Logger,
    },
    lampo_common::secp256k1,
};

use crate::ln::route::secp256k1::PublicKey;
#[cfg(feature = "rgb")]
pub use {
    rgb_lampo_common::error,
    rgb_lampo_common::ldk::blinded_path::payment::ForwardNode,
    rgb_lampo_common::ldk::blinded_path::payment::ForwardTlvs,
    rgb_lampo_common::ldk::blinded_path::payment::PaymentConstraints,
    rgb_lampo_common::ldk::blinded_path::payment::PaymentRelay,
    rgb_lampo_common::ldk::blinded_path::payment::ReceiveTlvs,
    rgb_lampo_common::ldk::blinded_path::BlindedPath,
    rgb_lampo_common::ldk::ln::channelmanager::ChannelDetails,
    rgb_lampo_common::ldk::ln::channelmanager::MIN_FINAL_CLTV_EXPIRY_DELTA,
    rgb_lampo_common::ldk::ln::features::BlindedHopFeatures,
    rgb_lampo_common::ldk::ln::msgs::LightningError,
    rgb_lampo_common::ldk::offers::invoice::BlindedPayInfo,
    rgb_lampo_common::ldk::onion_message::messenger,
    rgb_lampo_common::ldk::onion_message::messenger::MessageRouter,
    rgb_lampo_common::ldk::routing::gossip::NodeId,
    rgb_lampo_common::ldk::routing::router::find_route,
    rgb_lampo_common::ldk::routing::router::InFlightHtlcs,
    rgb_lampo_common::ldk::routing::router::Route,
    rgb_lampo_common::ldk::routing::router::RouteParameters,
    rgb_lampo_common::ldk::routing::router::Router,
    rgb_lampo_common::ldk::routing::router::ScorerAccountingForInFlightHtlcs,
    rgb_lampo_common::ldk::{
        routing::{
            gossip::NetworkGraph,
            scoring::{LockableScore, ScoreLookUp},
        },
        sign::EntropySource,
        util::logger::Logger,
    },
    rgb_lampo_common::secp256k1,
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

    // Not present inside `lightning`` the RGB guys are using
    #[cfg(feature = "vanilla")]
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
    fn create_blinded_paths<T: secp256k1::Signing + secp256k1::Verification>(
        &self,
        recipient: secp256k1::PublicKey,
        peers: Vec<secp256k1::PublicKey>,
        secp_ctx: &secp256k1::Secp256k1<T>,
    ) -> Result<Vec<BlindedPath>, ()> {
        self.message_router
            .create_blinded_paths(recipient, peers, secp_ctx)
    }

    fn find_path(
        &self,
        sender: secp256k1::PublicKey,
        peers: Vec<secp256k1::PublicKey>,
        destination: messenger::Destination,
    ) -> Result<messenger::OnionMessagePath, ()> {
        self.message_router.find_path(sender, peers, destination)
    }
}
