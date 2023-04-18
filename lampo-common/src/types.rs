//! Lampo Common T
use bitcoin::secp256k1::PublicKey;

pub type NodeId = PublicKey;
pub type ChannelId = [u8; 32];

pub enum ChannelState {
    Opening,
    Ready,
}
