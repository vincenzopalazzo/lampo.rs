//! Onion message routing for the lampo onion messenger.
use std::sync::Arc;

use lampo_common::bitcoin::secp256k1::{self, PublicKey, Secp256k1};
use lampo_common::ldk::blinded_path::message::{
    BlindedMessagePath, MessageContext, MessageForwardNode,
};
use lampo_common::ldk::blinded_path::IntroductionNode;
use lampo_common::ldk::onion_message::messenger::{
    Destination, MessageRouter, NodeIdMessageRouter, OnionMessagePath,
};
use lampo_common::ldk::routing::gossip::NodeId;
use lampo_common::ldk::sign::ReceiveAuthKey;
use lampo_common::signer::LampoSigner;
use lampo_common::types::LampoGraph;

use crate::utils::logger::LampoLogger;

/// Routes onion messages like [`NodeIdMessageRouter`], and also reaches a
/// node that announces no address through a peer it has a channel with.
///
/// LDK only sends onion messages to peers or to nodes it can connect to.
/// A Core Lightning payer without `announce-addr` puts itself as the
/// introduction node of its reply path, so without this the invoice for
/// its `invoice_request` would never be sent.
pub struct LampoMessageRouter {
    graph: Arc<LampoGraph>,
    inner: NodeIdMessageRouter<Arc<LampoGraph>, Arc<LampoLogger>, Arc<dyn LampoSigner>>,
}

impl LampoMessageRouter {
    pub fn new(graph: Arc<LampoGraph>, entropy_source: Arc<dyn LampoSigner>) -> Self {
        let inner = NodeIdMessageRouter::new(graph.clone(), entropy_source);
        Self { graph, inner }
    }

    /// One of `peers` that has a channel with `node_id` in the network graph.
    fn peer_with_channel_to(&self, peers: &[PublicKey], node_id: &PublicKey) -> Option<PublicKey> {
        let graph = self.graph.read_only();
        let node = graph.node(&NodeId::from_pubkey(node_id))?;
        peers.iter().copied().find(|peer| {
            let peer = NodeId::from_pubkey(peer);
            node.channels
                .iter()
                .filter_map(|scid| graph.channel(*scid))
                .any(|channel| channel.node_one == peer || channel.node_two == peer)
        })
    }
}

impl MessageRouter for LampoMessageRouter {
    fn find_path(
        &self,
        sender: PublicKey,
        peers: Vec<PublicKey>,
        destination: Destination,
    ) -> Result<OnionMessagePath, ()> {
        if let Ok(path) = self
            .inner
            .find_path(sender, peers.clone(), destination.clone())
        {
            return Ok(path);
        }

        let mut destination = destination;
        destination.resolve(&self.graph.read_only());
        let first_node = match &destination {
            Destination::Node(node_id) => *node_id,
            Destination::BlindedPath(path) => match path.introduction_node() {
                IntroductionNode::NodeId(node_id) => *node_id,
                IntroductionNode::DirectedShortChannelId(..) => return Err(()),
            },
        };
        let peer = self.peer_with_channel_to(&peers, &first_node).ok_or(())?;
        log::debug!(
            target: "lampod",
            "reaching onion message node `{first_node}` through peer `{peer}`"
        );
        Ok(OnionMessagePath {
            intermediate_nodes: vec![peer],
            destination,
            first_node_addresses: vec![],
        })
    }

    fn create_blinded_paths<T: secp256k1::Signing + secp256k1::Verification>(
        &self,
        recipient: PublicKey,
        local_node_receive_key: ReceiveAuthKey,
        context: MessageContext,
        peers: Vec<MessageForwardNode>,
        secp_ctx: &Secp256k1<T>,
    ) -> Result<Vec<BlindedMessagePath>, ()> {
        self.inner
            .create_blinded_paths(recipient, local_node_receive_key, context, peers, secp_ctx)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lampo_common::bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};
    use lampo_common::bitcoin::Network;
    use lampo_common::keys::LampoKeysManager;
    use lampo_common::ldk::ln::msgs::UnsignedNodeAnnouncement;
    use lampo_common::ldk::onion_message::messenger::{Destination, MessageRouter};
    use lampo_common::ldk::routing::gossip::{NetworkGraph, NodeAlias, NodeId};
    use lampo_common::ldk::types::features::{ChannelFeatures, NodeFeatures};

    use super::LampoMessageRouter;
    use crate::utils::logger::LampoLogger;

    fn pubkey(byte: u8) -> PublicKey {
        PublicKey::from_secret_key(
            &Secp256k1::new(),
            &SecretKey::from_slice(&[byte; 32]).unwrap(),
        )
    }

    /// A router whose graph has a channel between `peer` and `payer`, and a
    /// node announcement from `payer` without addresses.
    fn router(peer: PublicKey, payer: PublicKey) -> LampoMessageRouter {
        let graph = Arc::new(NetworkGraph::new(Network::Regtest, Arc::new(LampoLogger)));
        let (peer, payer) = (NodeId::from_pubkey(&peer), NodeId::from_pubkey(&payer));
        graph
            .add_channel_from_partial_announcement(
                42,
                Some(100_000),
                1,
                ChannelFeatures::empty(),
                peer,
                payer,
            )
            .unwrap();
        let mut features = NodeFeatures::empty();
        features.set_onion_messages_optional();
        graph
            .update_node_from_unsigned_announcement(&UnsignedNodeAnnouncement {
                features,
                timestamp: 1,
                node_id: payer,
                rgb: [0; 3],
                alias: NodeAlias([0; 32]),
                addresses: vec![],
                excess_address_data: vec![],
                excess_data: vec![],
            })
            .unwrap();
        LampoMessageRouter::new(graph, Arc::new(LampoKeysManager::new(&[1; 32], 1, 2)))
    }

    #[test]
    fn reaches_node_without_address_through_peer() {
        let (us, peer, payer) = (pubkey(1), pubkey(2), pubkey(3));
        let router = router(peer, payer);

        let path = router
            .find_path(us, vec![peer], Destination::Node(payer))
            .expect("the payer is reachable through the peer");
        assert_eq!(path.intermediate_nodes, vec![peer]);
        assert_eq!(path.destination, Destination::Node(payer));
        assert!(path.first_node_addresses.is_empty());
    }

    #[test]
    fn needs_a_peer_with_a_channel_to_the_node() {
        let (us, peer, payer, other) = (pubkey(1), pubkey(2), pubkey(3), pubkey(4));
        let router = router(peer, payer);

        assert!(router
            .find_path(us, vec![other], Destination::Node(payer))
            .is_err());
    }
}
