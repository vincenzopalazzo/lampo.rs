//! Buffer for onion messages addressed to offline peers.
//!
//! A lampo node built with `OnionMessenger::new_with_offline_peer_interception`
//! (the `async-payments-role=server` configuration) receives
//! `Event::OnionMessageIntercepted` for messages whose next hop is offline.
//! The mailbox holds them until `Event::OnionMessagePeerConnected` fires and
//! they can be forwarded. Ported from ldk-node's
//! `src/payment/asynchronous/om_mailbox.rs`.
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::ldk::ln::msgs::OnionMessage;

pub struct OnionMessageMailbox {
    map: Mutex<HashMap<PublicKey, VecDeque<OnionMessage>>>,
}

impl OnionMessageMailbox {
    const MAX_MESSAGES_PER_PEER: usize = 30;
    const MAX_PEERS: usize = 300;

    pub fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::with_capacity(Self::MAX_PEERS)),
        }
    }

    /// Buffer a message for an offline peer. Bounds are enforced by dropping
    /// the peer's oldest messages and, past the peer limit, evicting the peer
    /// with the longest queue.
    pub fn onion_message_intercepted(&self, peer_node_id: PublicKey, message: OnionMessage) {
        let mut map = self
            .map
            .lock()
            // A poisoned lock means another thread panicked with the lock
            // held; the map is still usable, and losing buffered messages is
            // worse than recovering them.
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let queue = map.entry(peer_node_id).or_insert_with(VecDeque::new);
        if queue.len() >= Self::MAX_MESSAGES_PER_PEER {
            log::debug!(
                target: "lampo::om-mailbox",
                "mailbox full for `{peer_node_id}`; dropping oldest message"
            );
            queue.pop_front();
        }
        queue.push_back(message);
        log::debug!(
            target: "lampo::om-mailbox",
            "buffered onion message for offline peer `{peer_node_id}` (queue {})",
            queue.len()
        );

        if map.len() > Self::MAX_PEERS {
            let peer_to_remove = map
                .iter()
                .max_by_key(|(_, queue)| queue.len())
                .map(|(peer, _)| *peer);
            if let Some(peer) = peer_to_remove {
                log::debug!(
                    target: "lampo::om-mailbox",
                    "mailbox peer cap reached; evicting `{peer}`"
                );
                map.remove(&peer);
            }
        }
    }

    /// Drain all buffered messages for a peer that just connected.
    pub fn onion_message_peer_connected(&self, peer_node_id: PublicKey) -> Vec<OnionMessage> {
        let mut map = self
            .map
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let drained: Vec<OnionMessage> = map
            .remove(&peer_node_id)
            .map(|queue| queue.into())
            .unwrap_or_default();
        if !drained.is_empty() {
            log::debug!(
                target: "lampo::om-mailbox",
                "draining {} buffered onion message(s) for `{peer_node_id}`",
                drained.len()
            );
        }
        drained
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lampo_common::bitcoin::secp256k1::{Secp256k1, SecretKey};

    fn peer(seed: u16) -> PublicKey {
        let secp = Secp256k1::new();
        let mut bytes = [0u8; 32];
        bytes[30..].copy_from_slice(&(seed + 1).to_be_bytes());
        PublicKey::from_secret_key(&secp, &SecretKey::from_slice(&bytes).unwrap())
    }

    fn message(tag: u8) -> OnionMessage {
        // The mailbox never inspects the message; a default-ish value is fine.
        OnionMessage {
            blinding_point: peer(tag as u16),
            onion_routing_packet: lampo_common::ldk::onion_message::packet::Packet {
                version: 0,
                public_key: peer(tag as u16),
                hop_data: vec![tag; 8],
                hmac: [tag; 32],
            },
        }
    }

    #[test]
    fn buffers_and_drains_per_peer() {
        let mailbox = OnionMessageMailbox::new();
        let peer_a = peer(1);
        let peer_b = peer(2);
        mailbox.onion_message_intercepted(peer_a, message(1));
        mailbox.onion_message_intercepted(peer_a, message(2));
        mailbox.onion_message_intercepted(peer_b, message(3));

        let drained = mailbox.onion_message_peer_connected(peer_a);
        assert_eq!(drained.len(), 2);
        // Draining is destructive.
        assert!(mailbox.onion_message_peer_connected(peer_a).is_empty());
        assert_eq!(mailbox.onion_message_peer_connected(peer_b).len(), 1);
    }

    #[test]
    fn drops_oldest_messages_past_the_per_peer_cap() {
        let mailbox = OnionMessageMailbox::new();
        let peer_a = peer(1);
        for i in 0..(OnionMessageMailbox::MAX_MESSAGES_PER_PEER + 10) {
            mailbox.onion_message_intercepted(peer_a, message(i as u8));
        }
        let drained = mailbox.onion_message_peer_connected(peer_a);
        assert_eq!(drained.len(), OnionMessageMailbox::MAX_MESSAGES_PER_PEER);
        // The oldest messages were dropped: the first surviving tag is 10.
        assert_eq!(drained[0].onion_routing_packet.hop_data[0], 10);
    }

    #[test]
    fn evicts_the_longest_queue_past_the_peer_cap() {
        let mailbox = OnionMessageMailbox::new();
        let full_peer = peer(1);
        for i in 0..3 {
            mailbox.onion_message_intercepted(full_peer, message(i));
        }
        for i in 0..OnionMessageMailbox::MAX_PEERS {
            mailbox.onion_message_intercepted(peer(i as u16 + 2), message(4));
        }
        // The longest queue (full_peer, 3 messages) was evicted.
        assert!(mailbox.onion_message_peer_connected(full_peer).is_empty());
    }
}
