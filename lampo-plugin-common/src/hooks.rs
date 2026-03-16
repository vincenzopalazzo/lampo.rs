//! Hook point definitions.
//!
//! Hooks are synchronous interception points. When a hook fires,
//! the daemon blocks until all registered plugins respond.
use serde::{Deserialize, Serialize};

/// Well-known hook points in lampo.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPoint {
    /// Fired when a peer completes the handshake.
    PeerConnected,
    /// Fired when a channel open request is received.
    OpenChannel,
    /// Fired when an HTLC is received and claimable.
    HtlcAccepted,
    /// Fired before any RPC command is dispatched (meta-hook).
    RpcCommand,
    /// Fired when an invoice is being created.
    InvoiceCreation,
}

impl HookPoint {
    /// The wire method name for this hook (sent as JSON-RPC method).
    pub fn method_name(&self) -> &'static str {
        match self {
            Self::PeerConnected => "hook/peer_connected",
            Self::OpenChannel => "hook/openchannel",
            Self::HtlcAccepted => "hook/htlc_accepted",
            Self::RpcCommand => "hook/rpc_command",
            Self::InvoiceCreation => "hook/invoice_creation",
        }
    }

    /// Parse from a hook name string (as declared in manifest).
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "peer_connected" => Some(Self::PeerConnected),
            "openchannel" => Some(Self::OpenChannel),
            "htlc_accepted" => Some(Self::HtlcAccepted),
            "rpc_command" => Some(Self::RpcCommand),
            "invoice_creation" => Some(Self::InvoiceCreation),
            _ => None,
        }
    }
}
