//! Onion Messages feature implementation for Lampo.
use std::ops::Deref;

use lampo_common::error;
use lampo_common::ldk::blinded_path::BlindedPath;
use lampo_common::ldk::blinded_path::IntroductionNode;
use lampo_common::ldk::onion_message::messenger::Destination;
use lampo_common::ldk::onion_message::messenger::MessageRouter;
use lampo_common::ldk::onion_message::messenger::OnionMessagePath;
use lampo_common::ldk::routing::gossip::{NetworkGraph, NodeId};
use lampo_common::ldk::sign::EntropySource;
use lampo_common::ldk::util::logger::Logger;

pub struct LampoMsgRouter<G: Deref<Target = NetworkGraph<L>> + Clone, L: Deref, ES: Deref>
where
    L::Target: Logger,
    ES::Target: EntropySource,
{
    graph: G,
    keys: ES,
}

impl<G: Deref<Target = NetworkGraph<L>> + Clone, L: Deref, ES: Deref> LampoMsgRouter<G, L, ES>
where
    L::Target: Logger,
    ES::Target: EntropySource,
{
    pub fn new(graph: G, keys: ES) -> error::Result<Self> {
        Ok(Self { graph, keys })
    }
}

impl<G: Deref<Target = NetworkGraph<L>> + Clone, L: Deref, ES: Deref> MessageRouter
    for LampoMsgRouter<G, L, ES>
where
    L::Target: Logger,
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
        // Limit the number of blinded paths that are computed.
        const MAX_PATHS: usize = 3;

        let network_graph = self.graph.deref().read_only();
        let is_recipient_announced = network_graph
            .nodes()
            .contains_key(&NodeId::from_pubkey(&recipient));

        let peer_info = peers
            .iter()
            // Limit to peers with announced channels
            .filter_map(|pubkey| {
                network_graph
                    .node(&NodeId::from_pubkey(pubkey))
                    .map(|info| (*pubkey, info.channels.len()))
            })
            .collect::<Vec<_>>();
        let paths = peer_info
            .into_iter()
            .map(|(pubkey, _)| vec![pubkey, recipient])
            .map(|node_pks| BlindedPath::new_for_message(&node_pks, &*self.keys, secp_ctx))
            .take(MAX_PATHS)
            .collect::<Result<Vec<_>, _>>();

        // BOLT 12:
        // if it is connected only by private channels:
        //  - MUST include offer_paths containing one or more paths to the node from publicly reachable nodes.
        // otherwise:
        //  - MAY include offer_paths.
        // if it includes offer_paths:
        //  - SHOULD ignore any invoice_request which does not use the path.
        match paths {
            Ok(paths) if !paths.is_empty() => Ok(paths),
            _ => {
                if is_recipient_announced {
                    BlindedPath::one_hop_for_message(recipient, &*self.keys, secp_ctx)
                        .map(|path| vec![path])
                } else {
                    Err(())
                }
            }
        }
    }

    fn find_path(
        &self,
        _sender: lampo_common::secp256k1::PublicKey,
        peers: Vec<lampo_common::secp256k1::PublicKey>,
        destination: lampo_common::ldk::onion_message::messenger::Destination,
    ) -> Result<lampo_common::ldk::onion_message::messenger::OnionMessagePath, ()> {
        let first_node = match &destination {
            Destination::Node(node_id) => node_id,
            Destination::BlindedPath(BlindedPath {
                introduction_node, ..
            }) => match introduction_node {
                IntroductionNode::NodeId(node_id) => node_id,
                IntroductionNode::DirectedShortChannelId(..) => return Err(()),
            },
        };
        if peers.contains(&first_node) {
            Ok(OnionMessagePath {
                intermediate_nodes: vec![],
                destination,
                first_node_addresses: None,
            })
        } else {
            let network_graph = self.graph.deref().read_only();
            let node_announcement = network_graph
                .node(&NodeId::from_pubkey(&first_node))
                .and_then(|node_info| node_info.announcement_info.as_ref())
                .and_then(|announcement_info| announcement_info.announcement_message.as_ref())
                .map(|node_announcement| &node_announcement.contents);

            match node_announcement {
                Some(node_announcement) if node_announcement.features.supports_onion_messages() => {
                    let first_node_addresses = Some(node_announcement.addresses.clone());
                    Ok(OnionMessagePath {
                        intermediate_nodes: vec![],
                        destination,
                        first_node_addresses,
                    })
                }
                _ => Err(()),
            }
        }
    }
}
