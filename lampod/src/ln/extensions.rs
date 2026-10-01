//! The custom message handler installed in the peer manager: it owns no
//! protocol itself and routes every custom message, feature bit, outbound
//! queue and RPC method to the registered extensions.

use std::collections::HashMap;
use std::sync::Arc;

use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::error;
use lampo_common::extension::{
    CustomMessageExtension, ExtensionContext, PersistentPeer, RawCustomMessage,
};
use lampo_common::json;
use lampo_common::jsonrpc;
use lampo_common::ldk::ln::msgs::{DecodeError, Init, LightningError};
use lampo_common::ldk::ln::peer_handler::CustomMessageHandler;
use lampo_common::ldk::ln::wire::CustomMessageReader;
use lampo_common::ldk::types::features::{InitFeatures, NodeFeatures};
use lampo_common::ldk::types::payment::PaymentHash;
use lampo_common::ldk::util::ser::LengthLimitedRead;

pub struct CustomMessageDispatcher {
    extensions: Vec<Arc<dyn CustomMessageExtension>>,
    /// Message type id to the index of the extension owning it.
    owners: HashMap<u16, usize>,
}

impl CustomMessageDispatcher {
    /// Fails when two extensions claim the same message type.
    pub fn new(extensions: Vec<Arc<dyn CustomMessageExtension>>) -> error::Result<Self> {
        let mut owners = HashMap::new();
        for (index, extension) in extensions.iter().enumerate() {
            for type_id in extension.message_types() {
                if let Some(other) = owners.insert(type_id, index) {
                    error::bail!(
                        "message type {type_id} is claimed by both `{}` and `{}`",
                        extensions[other].name(),
                        extension.name()
                    );
                }
            }
        }
        Ok(Self { extensions, owners })
    }

    pub fn extensions(&self) -> &[Arc<dyn CustomMessageExtension>] {
        &self.extensions
    }

    pub fn persistent_peers(&self) -> Vec<PersistentPeer> {
        self.extensions
            .iter()
            .flat_map(|extension| extension.persistent_peers())
            .collect()
    }

    pub fn attach(&self, context: ExtensionContext) {
        for extension in &self.extensions {
            log::debug!(target: "lampo", "attaching extension `{}`", extension.name());
            extension.attach(context.clone());
        }
    }

    /// The largest skim any extension vouches for; zero when none does.
    pub fn counterparty_skim_budget_msat(
        &self,
        payment_hash: &PaymentHash,
        counterparties: &[Option<PublicKey>],
    ) -> u64 {
        self.extensions
            .iter()
            .map(|extension| extension.counterparty_skim_budget_msat(payment_hash, counterparties))
            .max()
            .unwrap_or(0)
    }

    /// Serve `method` from the first extension that knows it.
    pub async fn rpc(
        &self,
        method: &str,
        args: &json::Value,
    ) -> Result<Option<json::Value>, jsonrpc::Error> {
        for extension in &self.extensions {
            if let Some(response) = extension.rpc(method, args).await? {
                return Ok(Some(response));
            }
        }
        Ok(None)
    }
}

impl CustomMessageReader for CustomMessageDispatcher {
    type CustomMessage = RawCustomMessage;

    fn read<R: LengthLimitedRead>(
        &self,
        message_type: u16,
        buffer: &mut R,
    ) -> Result<Option<Self::CustomMessage>, DecodeError> {
        if !self.owners.contains_key(&message_type) {
            return Ok(None);
        }
        let mut payload = vec![0u8; buffer.remaining_bytes() as usize];
        buffer.read_exact(&mut payload)?;
        Ok(Some(RawCustomMessage {
            type_id: message_type,
            payload,
        }))
    }
}

impl CustomMessageHandler for CustomMessageDispatcher {
    fn handle_custom_message(
        &self,
        msg: RawCustomMessage,
        sender_node_id: PublicKey,
    ) -> Result<(), LightningError> {
        match self.owners.get(&msg.type_id) {
            Some(index) => self.extensions[*index].handle_message(msg, sender_node_id),
            // `read` only accepts owned types, so this cannot happen.
            None => {
                log::warn!(target: "lampo", "no extension owns message type {}", msg.type_id);
                Ok(())
            }
        }
    }

    fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, RawCustomMessage)> {
        self.extensions
            .iter()
            .flat_map(|extension| extension.drain_outbound())
            .collect()
    }

    fn peer_disconnected(&self, their_node_id: PublicKey) {
        for extension in &self.extensions {
            extension.peer_disconnected(their_node_id);
        }
    }

    fn peer_connected(
        &self,
        their_node_id: PublicKey,
        msg: &Init,
        inbound: bool,
    ) -> Result<(), ()> {
        for extension in &self.extensions {
            extension.peer_connected(their_node_id, msg, inbound);
        }
        Ok(())
    }

    fn provided_node_features(&self) -> NodeFeatures {
        NodeFeatures::empty()
    }

    fn provided_init_features(&self, their_node_id: PublicKey) -> InitFeatures {
        self.extensions
            .iter()
            .fold(InitFeatures::empty(), |features, extension| {
                features | extension.init_features(&their_node_id)
            })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Mutex;

    use lampo_common::ldk::util::ser::Writeable;

    use super::*;

    const PEER: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    /// Records what it was handed and echoes every message back.
    struct Echo {
        name: &'static str,
        types: Vec<u16>,
        bit: usize,
        seen: Mutex<Vec<RawCustomMessage>>,
        outbox: Mutex<Vec<(PublicKey, RawCustomMessage)>>,
    }

    impl Echo {
        fn new(name: &'static str, types: Vec<u16>, bit: usize) -> Arc<Self> {
            Arc::new(Self {
                name,
                types,
                bit,
                seen: Mutex::new(Vec::new()),
                outbox: Mutex::new(Vec::new()),
            })
        }
    }

    #[lampo_common::async_trait]
    impl CustomMessageExtension for Echo {
        fn name(&self) -> &'static str {
            self.name
        }

        fn message_types(&self) -> Vec<u16> {
            self.types.clone()
        }

        fn init_features(&self, _peer: &PublicKey) -> InitFeatures {
            let mut flags = vec![0u8; self.bit / 8 + 1];
            flags[self.bit / 8] |= 1 << (self.bit % 8);
            InitFeatures::from_le_bytes(flags)
        }

        fn handle_message(
            &self,
            msg: RawCustomMessage,
            sender: PublicKey,
        ) -> Result<(), LightningError> {
            self.outbox.lock().unwrap().push((sender, msg.clone()));
            self.seen.lock().unwrap().push(msg);
            Ok(())
        }

        fn drain_outbound(&self) -> Vec<(PublicKey, RawCustomMessage)> {
            std::mem::take(&mut self.outbox.lock().unwrap())
        }

        fn counterparty_skim_budget_msat(
            &self,
            _payment_hash: &PaymentHash,
            _counterparties: &[Option<PublicKey>],
        ) -> u64 {
            self.bit as u64
        }

        async fn rpc(
            &self,
            method: &str,
            _args: &json::Value,
        ) -> Result<Option<json::Value>, jsonrpc::Error> {
            Ok((method == self.name).then(|| json::json!({ "from": self.name })))
        }
    }

    fn peer() -> PublicKey {
        PublicKey::from_str(PEER).unwrap()
    }

    #[test]
    fn rejects_two_owners_for_one_type() {
        let a = Echo::new("a", vec![41041, 41042], 300);
        let b = Echo::new("b", vec![41042], 301);
        assert!(CustomMessageDispatcher::new(vec![a, b]).is_err());
    }

    #[test]
    fn routes_by_type_and_merges_features_and_outboxes() {
        let a = Echo::new("a", vec![41041], 300);
        let b = Echo::new("b", vec![35025], 303);
        let dispatcher = CustomMessageDispatcher::new(vec![a.clone(), b.clone()]).unwrap();

        // Unowned types are not ours; owned ones carry the rest of the buffer.
        assert_eq!(dispatcher.read(16, &mut &[1u8, 2][..]).unwrap(), None);
        assert_eq!(dispatcher.read(41042, &mut &[1u8, 2][..]).unwrap(), None);
        let msg = dispatcher
            .read(41041, &mut &[1u8, 2, 3][..])
            .unwrap()
            .unwrap();
        assert_eq!(msg.payload, vec![1, 2, 3]);
        assert_eq!(msg.encode(), vec![1, 2, 3]);

        dispatcher
            .handle_custom_message(msg.clone(), peer())
            .unwrap();
        assert_eq!(
            a.seen.lock().unwrap().as_slice(),
            std::slice::from_ref(&msg)
        );
        assert!(b.seen.lock().unwrap().is_empty());
        let queued = dispatcher.get_and_clear_pending_msg();
        assert_eq!(queued, vec![(peer(), msg)]);
        assert!(dispatcher.get_and_clear_pending_msg().is_empty());

        let features = dispatcher.provided_init_features(peer());
        let flags = features.le_flags();
        assert_eq!(flags[300 / 8] & (1 << (300 % 8)), 1 << (300 % 8));
        assert_eq!(flags[303 / 8] & (1 << (303 % 8)), 1 << (303 % 8));
        assert_eq!(dispatcher.provided_node_features(), NodeFeatures::empty());

        assert_eq!(
            dispatcher.counterparty_skim_budget_msat(&PaymentHash([0; 32]), &[]),
            303
        );
        assert!(dispatcher.persistent_peers().is_empty());
        assert!(dispatcher
            .peer_connected(peer(), &empty_init(), false)
            .is_ok());
        dispatcher.peer_disconnected(peer());
    }

    #[tokio::test]
    async fn serves_rpc_from_the_first_extension_that_knows_it() {
        let a = Echo::new("a", vec![41041], 300);
        let b = Echo::new("b", vec![35025], 303);
        let dispatcher = CustomMessageDispatcher::new(vec![a, b]).unwrap();
        let args = json::json!({});
        assert_eq!(
            dispatcher.rpc("b", &args).await.unwrap(),
            Some(json::json!({ "from": "b" }))
        );
        assert_eq!(dispatcher.rpc("c", &args).await.unwrap(), None);
    }

    fn empty_init() -> Init {
        Init {
            features: InitFeatures::empty(),
            networks: None,
            remote_network_address: None,
        }
    }
}
