//! Type-erased custom-message router.
//!
//! Fills LDK's `custom_message_handler` slot with a single concrete type so
//! plugins can register [`LampoMsgHandler`] implementations at runtime without
//! lampod naming those plugins.

use std::sync::{Arc, RwLock};

use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::ldk::ln::msgs::{DecodeError, Init, LightningError};
use lampo_common::ldk::ln::peer_handler::CustomMessageHandler;
use lampo_common::ldk::ln::wire::CustomMessageReader;
use lampo_common::ldk::types::features::{InitFeatures, NodeFeatures};
use lampo_common::ldk::util::ser::LengthLimitedRead;
use lampo_common::msg::{LampoMsgHandler, LampoWireMessage};

/// Runtime registry of custom-message plugins.
///
/// Clone the handler list before calling into a plugin so a plugin cannot
/// deadlock by re-entering [`Self::register`].
pub struct LampoCustomMessageRouter {
    handlers: RwLock<Vec<Arc<dyn LampoMsgHandler>>>,
}

impl LampoCustomMessageRouter {
    pub fn new() -> Self {
        Self {
            handlers: RwLock::new(Vec::new()),
        }
    }

    /// Register a plugin. Safe to call after the [`PeerManager`] is constructed.
    pub fn register(&self, handler: Arc<dyn LampoMsgHandler>) {
        // SAFETY: we do not panic while holding this lock.
        self.handlers.write().unwrap().push(handler);
    }

    fn handlers(&self) -> Vec<Arc<dyn LampoMsgHandler>> {
        // SAFETY: we do not panic while holding this lock.
        self.handlers.read().unwrap().clone()
    }
}

impl Default for LampoCustomMessageRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl CustomMessageReader for LampoCustomMessageRouter {
    type CustomMessage = LampoWireMessage;

    fn read<R: LengthLimitedRead>(
        &self,
        message_type: u16,
        buffer: &mut R,
    ) -> Result<Option<Self::CustomMessage>, DecodeError> {
        let claimed = self.handlers().iter().any(|h| h.handles(message_type));
        if !claimed {
            return Ok(None);
        }
        let remaining = buffer.remaining_bytes();
        if remaining > usize::MAX as u64 {
            return Err(DecodeError::InvalidValue);
        }
        let mut payload = vec![0u8; remaining as usize];
        buffer
            .read_exact(&mut payload)
            .map_err(|_| DecodeError::ShortRead)?;
        Ok(Some(LampoWireMessage {
            type_id: message_type,
            payload,
        }))
    }
}

impl CustomMessageHandler for LampoCustomMessageRouter {
    fn handle_custom_message(
        &self,
        msg: Self::CustomMessage,
        sender_node_id: PublicKey,
    ) -> Result<(), LightningError> {
        for handler in self.handlers() {
            if handler.handles(msg.type_id) {
                return handler.handle_custom_message(msg.type_id, &msg.payload, sender_node_id);
            }
        }
        Ok(())
    }

    fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, Self::CustomMessage)> {
        self.handlers()
            .iter()
            .flat_map(|h| h.get_and_clear_pending_msg())
            .collect()
    }

    fn peer_disconnected(&self, their_node_id: PublicKey) {
        for handler in self.handlers() {
            handler.peer_disconnected(their_node_id);
        }
    }

    fn peer_connected(
        &self,
        their_node_id: PublicKey,
        msg: &Init,
        inbound: bool,
    ) -> Result<(), ()> {
        let mut ok = Ok(());
        for handler in self.handlers() {
            if handler.peer_connected(their_node_id, msg, inbound).is_err() {
                ok = Err(());
            }
        }
        ok
    }

    fn provided_node_features(&self) -> NodeFeatures {
        let mut features = NodeFeatures::empty();
        for handler in self.handlers() {
            features |= handler.provided_node_features();
        }
        features
    }

    fn provided_init_features(&self, their_node_id: PublicKey) -> InitFeatures {
        let mut features = InitFeatures::empty();
        for handler in self.handlers() {
            features |= handler.provided_init_features(their_node_id);
        }
        features
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use lampo_common::bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};
    use lampo_common::ldk::ln::wire::CustomMessageReader;

    fn test_node_id() -> PublicKey {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[2u8; 32]).expect("static test key");
        PublicKey::from_secret_key(&secp, &sk)
    }

    struct EchoHandler {
        pending: Mutex<Vec<(PublicKey, LampoWireMessage)>>,
    }

    impl EchoHandler {
        fn new() -> Self {
            Self {
                pending: Mutex::new(Vec::new()),
            }
        }
    }

    impl LampoMsgHandler for EchoHandler {
        fn handles(&self, type_id: u16) -> bool {
            type_id == 42
        }

        fn handle_custom_message(
            &self,
            type_id: u16,
            payload: &[u8],
            sender_node_id: PublicKey,
        ) -> Result<(), LightningError> {
            self.pending.lock().unwrap().push((
                sender_node_id,
                LampoWireMessage {
                    type_id,
                    payload: payload.to_vec(),
                },
            ));
            Ok(())
        }

        fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, LampoWireMessage)> {
            self.pending.lock().unwrap().drain(..).collect()
        }

        fn peer_disconnected(&self, _their_node_id: PublicKey) {}

        fn peer_connected(
            &self,
            _their_node_id: PublicKey,
            _msg: &Init,
            _inbound: bool,
        ) -> Result<(), ()> {
            Ok(())
        }

        fn provided_node_features(&self) -> NodeFeatures {
            NodeFeatures::empty()
        }

        fn provided_init_features(&self, _their_node_id: PublicKey) -> InitFeatures {
            InitFeatures::empty()
        }
    }

    #[test]
    fn unknown_type_is_ignored_without_handlers() {
        let router = LampoCustomMessageRouter::new();
        let mut buf: &[u8] = b"hello";
        let msg = router.read(42, &mut buf).unwrap();
        assert!(msg.is_none());
    }

    #[test]
    fn registered_handler_round_trips_payload() {
        let router = LampoCustomMessageRouter::new();
        let echo = Arc::new(EchoHandler::new());
        router.register(echo.clone());

        let mut buf: &[u8] = b"hello";
        let msg = router.read(42, &mut buf).unwrap().expect("claimed type");
        assert_eq!(msg.type_id, 42);
        assert_eq!(msg.payload, b"hello");

        let sender = test_node_id();
        router.handle_custom_message(msg, sender).unwrap();
        let pending = router.get_and_clear_pending_msg();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, sender);
        assert_eq!(pending[0].1.payload, b"hello");
    }

    #[test]
    fn unclaimed_type_stays_none_after_registering_echo() {
        let router = LampoCustomMessageRouter::new();
        router.register(Arc::new(EchoHandler::new()));
        let mut buf: &[u8] = b"nope";
        let msg = router.read(99, &mut buf).unwrap();
        assert!(msg.is_none());
    }

    struct FeatureHandler {
        node: NodeFeatures,
        init: InitFeatures,
    }

    impl FeatureHandler {
        fn with_custom_bits(node_bit: usize, init_bit: usize) -> Self {
            let mut node = NodeFeatures::empty();
            node.set_optional_custom_bit(node_bit)
                .expect("custom node feature bit");
            let mut init = InitFeatures::empty();
            init.set_optional_custom_bit(init_bit)
                .expect("custom init feature bit");
            Self { node, init }
        }
    }

    impl LampoMsgHandler for FeatureHandler {
        fn handles(&self, _type_id: u16) -> bool {
            false
        }

        fn handle_custom_message(
            &self,
            _type_id: u16,
            _payload: &[u8],
            _sender_node_id: PublicKey,
        ) -> Result<(), LightningError> {
            Ok(())
        }

        fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, LampoWireMessage)> {
            Vec::new()
        }

        fn peer_disconnected(&self, _their_node_id: PublicKey) {}

        fn peer_connected(
            &self,
            _their_node_id: PublicKey,
            _msg: &Init,
            _inbound: bool,
        ) -> Result<(), ()> {
            Ok(())
        }

        fn provided_node_features(&self) -> NodeFeatures {
            self.node.clone()
        }

        fn provided_init_features(&self, _their_node_id: PublicKey) -> InitFeatures {
            self.init.clone()
        }
    }

    #[test]
    fn provided_features_are_ored_across_handlers() {
        let router = LampoCustomMessageRouter::new();
        router.register(Arc::new(FeatureHandler::with_custom_bits(1000, 1000)));
        router.register(Arc::new(FeatureHandler::with_custom_bits(1002, 1002)));

        let mut expected_node = NodeFeatures::empty();
        expected_node.set_optional_custom_bit(1000).unwrap();
        expected_node.set_optional_custom_bit(1002).unwrap();
        assert_eq!(router.provided_node_features(), expected_node);

        let mut expected_init = InitFeatures::empty();
        expected_init.set_optional_custom_bit(1000).unwrap();
        expected_init.set_optional_custom_bit(1002).unwrap();
        assert_eq!(router.provided_init_features(test_node_id()), expected_init);
    }
}
