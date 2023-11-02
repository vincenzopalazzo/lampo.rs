//! Lampo Common Types
use crate::bitcoin::secp256k1::PublicKey;
use crate::ldk;

pub type NodeId = PublicKey;
pub type ChannelId = ldk::ln::ChannelId;

#[derive(Debug, Clone)]
pub enum ChannelState {
    Opening,
    Ready,
    OpeningError,
}
