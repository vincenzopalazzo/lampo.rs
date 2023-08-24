use crate::bitcoin::{OutPoint, Transaction};
use crate::ldk::ln::features::ChannelTypeFeatures;
use crate::types::NodeId;

#[derive(Clone, Debug)]
pub enum LightningEvent {
    // FIXME: add new peer model
    PeerConnect {
        counterparty_node_id: NodeId,
    },
    ChannelPending {
        counterparty_node_id: NodeId,
        funding_transaction: OutPoint,
    },
    ChannelReady {
        counterparty_node_id: NodeId,
        channel_id: [u8; 32],
        channel_type: ChannelTypeFeatures,
    },
    FundingChannelStart {
        counterparty_node_id: NodeId,
        temporary_channel_id: [u8; 32],
        channel_value_satoshis: u64,
    },
    FundingChannelEnd {
        counterparty_node_id: NodeId,
        temporary_channel_id: [u8; 32],
        channel_value_satoshis: u64,
        funding_transaction: Transaction,
    },
}
