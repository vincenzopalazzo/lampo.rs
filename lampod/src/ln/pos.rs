//! BLIP-0056 Point-of-Sale payment notifications.
//!
//! This module implements the custom onion messages defined by BLIP-0056 so
//! that a Point-of-Sale (PoS) node can receive `payment_notification` messages
//! from a merchant and reply with `notification_ack`/`notification_nack`, and
//! so that a merchant can send those notifications.
//!
//! The custom message handler is plugged into the LDK [`OnionMessenger`] in
//! place of the default `IgnoringMessageHandler`, see
//! [`crate::ln::peer_manager`].
//!
//! Author: Vincenzo Palazzo <vincenzopalazzo@member.fsf.org>
//!
//! NOTE: the TLV type numbers below are provisional. BLIP-0056 does not yet
//! assign final values; see `TODO.md` (TODO-1).
use std::sync::Arc;
use std::sync::Mutex;

use lampo_common::bitcoin::hashes::sha256::Hash as Sha256;
use lampo_common::bitcoin::hashes::Hash;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::{Emitter, Event};
use lampo_common::ldk::io;
use lampo_common::ldk::ln::msgs::DecodeError;
use lampo_common::ldk::onion_message::messenger::{
    CustomOnionMessageHandler, MessageSendInstructions, Responder, ResponseInstruction,
};
use lampo_common::ldk::onion_message::packet::OnionMessageContents;
use lampo_common::ldk::util::ser::{Readable, Writeable, Writer};

use crate::utils::logger::LampoLogger;

/// Provisional onion-message TLV type for `payment_notification`.
pub const POS_PAYMENT_NOTIFICATION_TYPE: u64 = 60_061;
/// Provisional onion-message TLV type for `notification_ack`.
pub const POS_NOTIFICATION_ACK_TYPE: u64 = 60_063;
/// Provisional onion-message TLV type for `notification_nack`.
pub const POS_NOTIFICATION_NACK_TYPE: u64 = 60_065;

/// The BLIP-0056 custom onion messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PosMessage {
    /// Merchant -> PoS: a payment for an order has been received.
    PaymentNotification {
        payment_hash: [u8; 32],
        preimage: [u8; 32],
        amount_msat: u64,
    },
    /// PoS -> Merchant: positive acknowledgement of a notification.
    ///
    /// BLIP-0056 leaves the ack payload unspecified; we carry the
    /// `payment_hash` so the merchant can correlate the reply. See `TODO.md`.
    NotificationAck { payment_hash: [u8; 32] },
    /// PoS -> Merchant: negative acknowledgement (verification failed).
    NotificationNack { payment_hash: [u8; 32] },
}

impl OnionMessageContents for PosMessage {
    fn tlv_type(&self) -> u64 {
        match self {
            PosMessage::PaymentNotification { .. } => POS_PAYMENT_NOTIFICATION_TYPE,
            PosMessage::NotificationAck { .. } => POS_NOTIFICATION_ACK_TYPE,
            PosMessage::NotificationNack { .. } => POS_NOTIFICATION_NACK_TYPE,
        }
    }

    fn msg_type(&self) -> &'static str {
        match self {
            PosMessage::PaymentNotification { .. } => "pos_payment_notification",
            PosMessage::NotificationAck { .. } => "pos_notification_ack",
            PosMessage::NotificationNack { .. } => "pos_notification_nack",
        }
    }
}

impl Writeable for PosMessage {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        match self {
            PosMessage::PaymentNotification {
                payment_hash,
                preimage,
                amount_msat,
            } => {
                payment_hash.write(w)?;
                preimage.write(w)?;
                amount_msat.write(w)?;
            }
            PosMessage::NotificationAck { payment_hash }
            | PosMessage::NotificationNack { payment_hash } => {
                payment_hash.write(w)?;
            }
        }
        Ok(())
    }
}

impl PosMessage {
    /// Read a `PosMessage` body of the given TLV `message_type` from `buffer`.
    ///
    /// Returns `Ok(None)` for unknown types so the messenger can ignore them.
    pub fn read<R: io::Read>(
        message_type: u64,
        buffer: &mut R,
    ) -> Result<Option<Self>, DecodeError> {
        match message_type {
            POS_PAYMENT_NOTIFICATION_TYPE => {
                let payment_hash: [u8; 32] = Readable::read(buffer)?;
                let preimage: [u8; 32] = Readable::read(buffer)?;
                let amount_msat: u64 = Readable::read(buffer)?;
                Ok(Some(PosMessage::PaymentNotification {
                    payment_hash,
                    preimage,
                    amount_msat,
                }))
            }
            POS_NOTIFICATION_ACK_TYPE => {
                let payment_hash: [u8; 32] = Readable::read(buffer)?;
                Ok(Some(PosMessage::NotificationAck { payment_hash }))
            }
            POS_NOTIFICATION_NACK_TYPE => {
                let payment_hash: [u8; 32] = Readable::read(buffer)?;
                Ok(Some(PosMessage::NotificationNack { payment_hash }))
            }
            _ => Ok(None),
        }
    }
}

/// `true` if `sha256(preimage) == payment_hash`.
pub fn verify_preimage(payment_hash: &[u8; 32], preimage: &[u8; 32]) -> bool {
    Sha256::hash(preimage).to_byte_array() == *payment_hash
}

/// Custom onion-message handler implementing the BLIP-0056 PoS role.
///
/// On the PoS side it verifies incoming `payment_notification`s and replies
/// with `notification_ack`/`notification_nack`. On the merchant side, messages
/// queued via [`PosOnionHandler::enqueue`] are drained by the
/// [`OnionMessenger`] through [`CustomOnionMessageHandler::release_pending_custom_messages`].
pub struct PosOnionHandler {
    emitter: Emitter<Event>,
    #[allow(dead_code)]
    logger: Arc<LampoLogger>,
    /// Outbound messages waiting to be sent by the messenger (merchant side).
    pending: Mutex<Vec<(PosMessage, MessageSendInstructions)>>,
}

impl PosOnionHandler {
    pub fn new(emitter: Emitter<Event>, logger: Arc<LampoLogger>) -> Self {
        Self {
            emitter,
            logger,
            pending: Mutex::new(Vec::new()),
        }
    }

    /// Queue a message to be sent on the next messenger processing cycle.
    pub fn enqueue(&self, message: PosMessage, instructions: MessageSendInstructions) {
        // SAFETY: the mutex is only held for the push, no panics in between.
        self.pending.lock().unwrap().push((message, instructions));
    }
}

impl CustomOnionMessageHandler for PosOnionHandler {
    type CustomMessage = PosMessage;

    fn handle_custom_message(
        &self,
        msg: PosMessage,
        _context: Option<Vec<u8>>,
        responder: Option<Responder>,
    ) -> Option<(PosMessage, ResponseInstruction)> {
        match msg {
            PosMessage::PaymentNotification {
                payment_hash,
                preimage,
                amount_msat,
            } => {
                // FIXME(TODO-2): the expected amount/order id will be read from
                // `_context` (the PoS-only MessageContext) in a follow-up.
                let verified = verify_preimage(&payment_hash, &preimage);
                log::info!(
                    target: "lampo::pos",
                    "received payment_notification hash={} amount={}msat verified={}",
                    hex::encode(payment_hash), amount_msat, verified
                );
                self.emitter
                    .emit(Event::Lightning(LightningEvent::PosPaymentNotified {
                        payment_hash: hex::encode(payment_hash),
                        amount_msat,
                        verified,
                    }));
                match responder {
                    Some(responder) => {
                        let reply = if verified {
                            PosMessage::NotificationAck { payment_hash }
                        } else {
                            PosMessage::NotificationNack { payment_hash }
                        };
                        Some((reply, responder.respond()))
                    }
                    None => {
                        log::warn!(
                            target: "lampo::pos",
                            "payment_notification had no reply path, cannot ack"
                        );
                        None
                    }
                }
            }
            PosMessage::NotificationAck { payment_hash } => {
                log::info!(
                    target: "lampo::pos",
                    "merchant received notification_ack for {}", hex::encode(payment_hash)
                );
                self.emitter
                    .emit(Event::Lightning(LightningEvent::PosNotificationAck {
                        payment_hash: hex::encode(payment_hash),
                        acked: true,
                    }));
                None
            }
            PosMessage::NotificationNack { payment_hash } => {
                log::warn!(
                    target: "lampo::pos",
                    "merchant received notification_nack for {}", hex::encode(payment_hash)
                );
                self.emitter
                    .emit(Event::Lightning(LightningEvent::PosNotificationAck {
                        payment_hash: hex::encode(payment_hash),
                        acked: false,
                    }));
                None
            }
        }
    }

    fn read_custom_message<R: io::Read>(
        &self,
        message_type: u64,
        buffer: &mut R,
    ) -> Result<Option<PosMessage>, DecodeError> {
        PosMessage::read(message_type, buffer)
    }

    fn release_pending_custom_messages(&self) -> Vec<(PosMessage, MessageSendInstructions)> {
        let mut pending = self.pending.lock().unwrap();
        std::mem::take(&mut *pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lampo_common::ldk::util::ser::Writeable;

    fn roundtrip(msg: PosMessage) {
        let bytes = msg.encode();
        let mut cursor = io::Cursor::new(&bytes);
        let decoded = PosMessage::read(msg.tlv_type(), &mut cursor)
            .expect("decode ok")
            .expect("known type");
        assert_eq!(msg, decoded);
    }

    #[test]
    fn payment_notification_roundtrip() {
        roundtrip(PosMessage::PaymentNotification {
            payment_hash: [7u8; 32],
            preimage: [9u8; 32],
            amount_msat: 123_456,
        });
    }

    #[test]
    fn ack_nack_roundtrip() {
        roundtrip(PosMessage::NotificationAck {
            payment_hash: [1u8; 32],
        });
        roundtrip(PosMessage::NotificationNack {
            payment_hash: [2u8; 32],
        });
    }

    #[test]
    fn unknown_type_is_none() {
        let mut cursor = io::Cursor::new(vec![0u8; 32]);
        assert!(PosMessage::read(42, &mut cursor).unwrap().is_none());
    }

    #[test]
    fn preimage_verification() {
        let preimage = [3u8; 32];
        let hash = Sha256::hash(&preimage).to_byte_array();
        assert!(verify_preimage(&hash, &preimage));
        assert!(!verify_preimage(&[0u8; 32], &preimage));
    }
}
