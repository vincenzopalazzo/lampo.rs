//! What the Phoenix extension reports on the node's event bus. The bus
//! carries a generic [`LightningEvent::Extension`]; these are its typed
//! payloads, serialized as JSON with a `kind` tag.

use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::json;
use lampo_common::serde::{Deserialize, Serialize};
use lampo_common::types::NodeId;

/// The `extension` name every Phoenix event carries.
pub const EXTENSION: &str = "phoenix-lsp";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(crate = "lampo_common::serde", tag = "kind", rename_all = "snake_case")]
pub enum PhoenixLspEvent {
    /// The configured Phoenix LSP completed its handshake with this node.
    Connected {
        counterparty_node_id: NodeId,
        /// The LSP advertised on-the-fly funding (bit 560 or 561).
        on_the_fly_funding: bool,
        /// The LSP advertised funding fee credit (bit 562 or 563).
        funding_fee_credit: bool,
    },
    /// The LSP proposed an HTLC that needs liquidity (`will_add_htlc`).
    /// Nothing is funded yet: the liquidity policy only decided, and
    /// `decision` says what it decided.
    WillAddHtlc {
        /// Hex id of the proposal.
        id: String,
        amount_msat: u64,
        /// Hex payment hash.
        payment_hash: String,
        cltv_expiry: u32,
        decision: String,
    },
    /// The LSP gave up on funding (`cancel_on_the_fly_funding`).
    FundingCancelled {
        /// Hex channel id.
        channel_id: String,
        /// Hex payment hashes.
        payment_hashes: Vec<String>,
        reason: String,
    },
    /// The LSP answered a `dns_address_request`.
    DnsAddress {
        /// The BIP 353 address, `user@domain`.
        address: String,
    },
    /// A peer this node treats as its LSP asked for a BIP 353 address
    /// (`dns_address_request`). Lampo is not an LSP and does not answer;
    /// integration tests reply from a hook.
    DnsAddressRequest {
        counterparty_node_id: NodeId,
        /// Hex offer TLV stream.
        offer: String,
        language: String,
    },
}

impl PhoenixLspEvent {
    /// The `kind` tag of this event.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Connected { .. } => "connected",
            Self::WillAddHtlc { .. } => "will_add_htlc",
            Self::FundingCancelled { .. } => "funding_cancelled",
            Self::DnsAddress { .. } => "dns_address",
            Self::DnsAddressRequest { .. } => "dns_address_request",
        }
    }

    /// Wrap this event for the node's bus.
    pub fn into_event(self) -> Event {
        let kind = self.kind().to_owned();
        // SAFETY: every variant is plain data with string, integer, bool
        // and public key fields, all of which serialize.
        let payload = json::to_value(&self).expect("phoenix event serializes");
        Event::Lightning(LightningEvent::Extension {
            extension: EXTENSION.to_owned(),
            kind,
            payload,
        })
    }

    /// The Phoenix event inside `event`, if it is one.
    pub fn from_event(event: &Event) -> Option<Self> {
        let Event::Lightning(LightningEvent::Extension {
            extension, payload, ..
        }) = event
        else {
            return None;
        };
        if extension != EXTENSION {
            return None;
        }
        json::from_value(payload.clone()).ok()
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn round_trips_through_the_bus_with_its_kind() {
        let node_id =
            NodeId::from_str("03933884aaf1d6b108397e5efe5c86bcf2d8ca8d2f700eda99db9214fc2712b134")
                .unwrap();
        let event = PhoenixLspEvent::DnsAddressRequest {
            counterparty_node_id: node_id,
            offer: "00".to_owned(),
            language: "it".to_owned(),
        };
        let on_bus = event.clone().into_event();
        match &on_bus {
            Event::Lightning(LightningEvent::Extension {
                extension,
                kind,
                payload,
            }) => {
                assert_eq!(extension, EXTENSION);
                assert_eq!(kind, "dns_address_request");
                assert_eq!(payload["kind"], "dns_address_request");
                assert_eq!(payload["language"], "it");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(PhoenixLspEvent::from_event(&on_bus), Some(event));

        let foreign = Event::Lightning(LightningEvent::Extension {
            extension: "other".to_owned(),
            kind: "dns_address".to_owned(),
            payload: json::json!({ "kind": "dns_address", "address": "a@b" }),
        });
        assert_eq!(PhoenixLspEvent::from_event(&foreign), None);
    }
}
