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
        // if the payment failed, we can provide a reason
        // to help the user understand what went wrong.
        reason: Option<String>,
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
    /// BLIP-0056: a PoS node received and verified a `payment_notification`.
    PosPaymentNotified {
        payment_hash: String,
        amount_msat: u64,
        /// `true` if `sha256(preimage) == payment_hash`.
        verified: bool,
    },
    /// BLIP-0056: a merchant received a `notification_ack`/`notification_nack`.
    PosNotificationAck {
        payment_hash: String,
        /// `true` for an ack, `false` for a nack.
        acked: bool,
    },
}
