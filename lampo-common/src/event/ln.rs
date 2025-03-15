use lightning::types::features::ChannelTypeFeatures;

use crate::bitcoin::{OutPoint, Transaction};
use crate::model::response::{PaymentHop, PaymentState};
use crate::types::{ChannelId, NodeId};

#[derive(Clone, Debug)]
pub enum LightningEvent {
    // FIXME: add new peer event
    PeerConnect {
        counterparty_node_id: NodeId,
    },
    ChannelPending {
        counterparty_node_id: NodeId,
        funding_transaction: OutPoint,
    },
    ChannelReady {
        counterparty_node_id: NodeId,
        channel_id: ChannelId,
        channel_type: ChannelTypeFeatures,
    },
    FundingChannelStart {
        counterparty_node_id: NodeId,
        temporary_channel_id: ChannelId,
        channel_value_satoshis: u64,
    },
    FundingChannelEnd {
        counterparty_node_id: NodeId,
        temporary_channel_id: ChannelId,
        channel_value_satoshis: u64,
        funding_transaction: Transaction,
    },
    PaymentEvent {
        state: PaymentState,
        payment_hash: Option<String>,
        path: Vec<PaymentHop>,
    },
    ChannelEvent {
        state: String,
        message: String,
    },
    CloseChannelEvent {
        channel_id: String,
        message: String,
        counterparty_node_id: Option<String>,
        funding_utxo: Option<String>,
    },
}
