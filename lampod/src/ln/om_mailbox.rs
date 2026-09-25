//! Buffer for onion messages addressed to offline peers.
//!
//! A lampo node built with `OnionMessenger::new_with_offline_peer_interception`
//! (the `async-payments-role=server` configuration) receives
//! `Event::OnionMessageIntercepted` for messages whose next hop is offline.
//! The mailbox holds them until `Event::OnionMessagePeerConnected` fires and
//! they can be forwarded. Ported from ldk-node's
//! `src/payment/asynchronous/om_mailbox.rs`.
//!
//! Queues are written to the filesystem store so a server restart does not
//! drop in-flight `HeldHtlcAvailable` (and similar) messages. Layout:
//! `om_mailbox/<hex compressed pubkey>` — a `Writeable` `Vec<OnionMessage>`.
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::hex;
use lampo_common::ldk::io::ErrorKind;
use lampo_common::ldk::ln::msgs::OnionMessage;
use lampo_common::ldk::util::persist::KVStoreSync;
use lampo_common::ldk::util::ser::{LengthReadable, Writeable};

use lampo_common::persist::LampoPersistenceBackend;

const MAILBOX_NAMESPACE: &str = "om_mailbox";
const QUEUE_VERSION: u8 = 1;

pub struct OnionMessageMailbox {
    map: Mutex<HashMap<PublicKey, VecDeque<OnionMessage>>>,
    persister: Option<Arc<dyn LampoPersistenceBackend>>,
}

impl OnionMessageMailbox {
    const MAX_MESSAGES_PER_PEER: usize = 30;
    const MAX_PEERS: usize = 300;

    pub fn new() -> Self {
        Self::with_store(None)
    }

    pub fn with_store(persister: Option<Arc<dyn LampoPersistenceBackend>>) -> Self {
        let mut map = HashMap::with_capacity(Self::MAX_PEERS);
        if let Some(store) = persister.as_ref() {
            load_queues(store.as_ref(), &mut map);
        }
        Self {
            map: Mutex::new(map),
            persister,
        }
    }

    /// How many messages are buffered for `peer_node_id`.
    pub fn queued(&self, peer_node_id: PublicKey) -> usize {
        let map = self
            .map
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        map.get(&peer_node_id).map(|queue| queue.len()).unwrap_or(0)
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
        let queue_snapshot: Vec<OnionMessage> = queue.iter().cloned().collect();

        let evicted = if map.len() > Self::MAX_PEERS {
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
                Some(peer)
            } else {
                None
            }
        } else {
            None
        };
        drop(map);

        if evicted != Some(peer_node_id) {
            self.persist_queue(peer_node_id, &queue_snapshot);
        }
        if let Some(peer) = evicted {
            self.remove_queue(peer);
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
        drop(map);
        if !drained.is_empty() {
            log::debug!(
                target: "lampo::om-mailbox",
                "draining {} buffered onion message(s) for `{peer_node_id}`",
                drained.len()
            );
            self.remove_queue(peer_node_id);
        }
        drained
    }

    fn persist_queue(&self, peer_node_id: PublicKey, queue: &[OnionMessage]) {
        let Some(persister) = &self.persister else {
            return;
        };
        let key = hex::encode(peer_node_id.serialize());
        if let Err(err) = persister.write(MAILBOX_NAMESPACE, "", &key, encode_queue(queue)) {
            log::error!(
                target: "lampo::om-mailbox",
                "failed to persist mailbox for `{peer_node_id}`: {err}"
            );
        }
    }

    fn remove_queue(&self, peer_node_id: PublicKey) {
        let Some(persister) = &self.persister else {
            return;
        };
        let key = hex::encode(peer_node_id.serialize());
        if let Err(err) = persister.remove(MAILBOX_NAMESPACE, "", &key, false) {
            log::error!(
                target: "lampo::om-mailbox",
                "failed to remove mailbox for `{peer_node_id}`: {err}"
            );
        }
    }
}

fn load_queues(
    persister: &dyn LampoPersistenceBackend,
    map: &mut HashMap<PublicKey, VecDeque<OnionMessage>>,
) {
    let keys = match persister.list(MAILBOX_NAMESPACE, "") {
        Ok(keys) => keys,
        Err(err) => {
            log::error!(target: "lampo::om-mailbox", "failed to list mailbox keys: {err}");
            return;
        }
    };
    for key in keys {
        if map.len() >= OnionMessageMailbox::MAX_PEERS {
            log::warn!(
                target: "lampo::om-mailbox",
                "mailbox peer cap reached while loading; leaving remaining keys on disk"
            );
            break;
        }
        match persister.read(MAILBOX_NAMESPACE, "", &key) {
            Ok(buf) => match decode_queue(&buf) {
                Ok(mut messages) => {
                    let Ok(pk_bytes) = hex::decode(&key) else {
                        drop_corrupt(persister, &key, "key is not hex");
                        continue;
                    };
                    let Ok(peer) = PublicKey::from_slice(&pk_bytes) else {
                        drop_corrupt(persister, &key, "key is not a public key");
                        continue;
                    };
                    if messages.len() > OnionMessageMailbox::MAX_MESSAGES_PER_PEER {
                        let skip = messages.len() - OnionMessageMailbox::MAX_MESSAGES_PER_PEER;
                        messages.drain(..skip);
                    }
                    if !messages.is_empty() {
                        map.insert(peer, messages.into());
                    }
                }
                Err(err) => drop_corrupt(persister, &key, &err),
            },
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                log::error!(
                    target: "lampo::om-mailbox",
                    "failed to read mailbox `{key}`: {err}"
                );
            }
        }
    }
}

/// version, big-endian u16 count, then each message as a u32 length
/// prefix plus its `Writeable` encoding. `OnionMessage` is only
/// `LengthReadable`, so the vec cannot delimit itself.
fn encode_queue(queue: &[OnionMessage]) -> Vec<u8> {
    let mut buf = vec![QUEUE_VERSION];
    buf.extend_from_slice(&(queue.len() as u16).to_be_bytes());
    for message in queue {
        let encoded = message.encode();
        buf.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
        buf.extend_from_slice(&encoded);
    }
    buf
}

fn decode_queue(buf: &[u8]) -> Result<Vec<OnionMessage>, String> {
    if buf.len() < 3 {
        return Err(format!("mailbox record too short: {} bytes", buf.len()));
    }
    if buf[0] != QUEUE_VERSION {
        return Err(format!("unsupported mailbox record version {}", buf[0]));
    }
    let count = u16::from_be_bytes([buf[1], buf[2]]) as usize;
    let mut rest = &buf[3..];
    let mut messages = Vec::with_capacity(count);
    for _ in 0..count {
        if rest.len() < 4 {
            return Err("mailbox record truncated at message length".to_owned());
        }
        let msg_len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        rest = &rest[4..];
        if rest.len() < msg_len {
            return Err(format!(
                "mailbox record truncated: want {msg_len} message bytes, have {}",
                rest.len()
            ));
        }
        let message = OnionMessage::read_from_fixed_length_buffer(&mut &rest[..msg_len])
            .map_err(|err| format!("decoding onion message: {err:?}"))?;
        rest = &rest[msg_len..];
        messages.push(message);
    }
    Ok(messages)
}

fn drop_corrupt(persister: &dyn LampoPersistenceBackend, key: &str, reason: &str) {
    log::error!(
        target: "lampo::om-mailbox",
        "dropping corrupt mailbox `{key}`: {reason}"
    );
    if let Err(err) = persister.remove(MAILBOX_NAMESPACE, "", key, false) {
        log::error!(
            target: "lampo::om-mailbox",
            "failed to remove corrupt mailbox `{key}`: {err}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lampo_common::bitcoin::secp256k1::{Secp256k1, SecretKey};
    use lampo_common::persist::FsPersistence;

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

    fn temp_store() -> (Arc<dyn LampoPersistenceBackend>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "lampo-om-mailbox-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (Arc::new(FsPersistence::new(dir.clone())), dir)
    }

    #[test]
    fn buffers_and_drains_per_peer() {
        let mailbox = OnionMessageMailbox::new();
        let peer_a = peer(1);
        let peer_b = peer(2);
        mailbox.onion_message_intercepted(peer_a, message(1));
        mailbox.onion_message_intercepted(peer_a, message(2));
        mailbox.onion_message_intercepted(peer_b, message(3));

        assert_eq!(mailbox.queued(peer_a), 2);
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

    #[test]
    fn reloads_queued_messages_from_the_store() {
        let (persister, dir) = temp_store();
        let peer_a = peer(1);
        {
            let mailbox = OnionMessageMailbox::with_store(Some(persister.clone()));
            mailbox.onion_message_intercepted(peer_a, message(7));
            mailbox.onion_message_intercepted(peer_a, message(8));
            assert_eq!(mailbox.queued(peer_a), 2);
        }
        let reloaded = OnionMessageMailbox::with_store(Some(persister));
        let drained = reloaded.onion_message_peer_connected(peer_a);
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].onion_routing_packet.hop_data[0], 7);
        assert_eq!(drained[1].onion_routing_packet.hop_data[0], 8);
        // Drain removes the on-disk queue.
        let empty =
            OnionMessageMailbox::with_store(Some(Arc::new(FsPersistence::new(dir.clone()))));
        assert!(empty.onion_message_peer_connected(peer_a).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
