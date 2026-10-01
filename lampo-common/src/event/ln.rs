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
    /// A payment settled. Carries the receipt, and is emitted before the
    /// terminal [`LightningEvent::PaymentEvent`] so a caller can pick both up.
    ///
    /// Kept separate from `PaymentEvent` because the receipt and the hop path
    /// arrive on different LDK events, and the receipt must not depend on the
    /// path being known.
    PaymentReceipt {
        /// Hex encoded payment id, to match the payment this belongs to.
        payment_id: String,
        /// Hex encoded preimage, the receipt of the payment.
        payment_preimage: String,
        /// Bech32 encoded BOLT 12 payer proof. Only set for offer payments:
        /// BOLT 11 and async (static invoice) payments cannot produce one.
        payer_proof: Option<String>,
    },
    PaymentEvent {
        state: PaymentState,
        /// Hex encoded payment id. Subscribers must filter on this: the bus
        /// broadcasts to every subscriber, so a concurrent `pay` would
        /// otherwise see another payment's result.
        payment_id: Option<String>,
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
    /// The configured Phoenix LSP completed its handshake with this node.
    PhoenixLspConnected {
        counterparty_node_id: NodeId,
        /// The LSP advertised on-the-fly funding (bit 560 or 561).
        on_the_fly_funding: bool,
        /// The LSP advertised funding fee credit (bit 562 or 563).
        funding_fee_credit: bool,
    },
    /// The Phoenix LSP proposed an HTLC that needs liquidity
    /// (`will_add_htlc`). Nothing is funded yet: the liquidity policy only
    /// decided, and `decision` says what it decided.
    PhoenixLspWillAddHtlc {
        /// Hex id of the proposal.
        id: String,
        amount_msat: u64,
        /// Hex payment hash.
        payment_hash: String,
        cltv_expiry: u32,
        decision: String,
    },
    /// The Phoenix LSP gave up on funding (`cancel_on_the_fly_funding`).
    PhoenixLspFundingCancelled {
        /// Hex channel id.
        channel_id: String,
        /// Hex payment hashes.
        payment_hashes: Vec<String>,
        reason: String,
    },
    /// The Phoenix LSP answered a `dns_address_request`.
    PhoenixLspDnsAddress {
        /// The BIP 353 address, `user@domain`.
        address: String,
    },
    /// A peer this node treats as its Phoenix LSP asked for a BIP 353
    /// address (`dns_address_request`). Lampo is not an LSP and does not
    /// answer; integration tests reply from a hook.
    PhoenixLspDnsAddressRequest {
        counterparty_node_id: NodeId,
        /// Hex offer TLV stream.
        offer: String,
        language: String,
    },
}
