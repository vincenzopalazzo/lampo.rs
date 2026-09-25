//! Notification topic constants.
//!
//! These are the well-known event topic strings that plugins can
//! subscribe to in their manifest. The daemon maps internal events
//! to these topics before forwarding to plugins.

/// Peer connected to our node.
pub const PEER_CONNECTED: &str = "peer_connected";

/// Channel funding confirmed, waiting for lock-in.
pub const CHANNEL_PENDING: &str = "channel_pending";

/// Channel is ready for use.
pub const CHANNEL_READY: &str = "channel_ready";

/// Channel funding started.
pub const FUNDING_CHANNEL_START: &str = "funding_channel_start";

/// Channel funding transaction created.
pub const FUNDING_CHANNEL_END: &str = "funding_channel_end";

/// Payment state changed (success or failure).
pub const PAYMENT: &str = "payment";

/// Channel state changed.
pub const CHANNEL_EVENT: &str = "channel_event";

/// Channel closed.
pub const CHANNEL_CLOSED: &str = "channel_closed";

/// New block received on-chain.
pub const NEW_BLOCK: &str = "new_block";

/// New best block header.
pub const NEW_BEST_BLOCK: &str = "new_best_block";

/// Fee estimation updated.
pub const FEE_ESTIMATION: &str = "fee_estimation";

/// Transaction confirmed on-chain.
pub const CONFIRMED_TRANSACTION: &str = "confirmed_transaction";

/// Wildcard — subscribe to all notifications.
pub const ALL: &str = "*";

/// All known topic names, for validation.
pub const KNOWN_TOPICS: &[&str] = &[
    PEER_CONNECTED,
    CHANNEL_PENDING,
    CHANNEL_READY,
    FUNDING_CHANNEL_START,
    FUNDING_CHANNEL_END,
    PAYMENT,
    CHANNEL_EVENT,
    CHANNEL_CLOSED,
    NEW_BLOCK,
    NEW_BEST_BLOCK,
    FEE_ESTIMATION,
    CONFIRMED_TRANSACTION,
    ALL,
];
