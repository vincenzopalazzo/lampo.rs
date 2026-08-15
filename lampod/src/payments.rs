//! In-memory payment history backing the `listpayments` RPC (issue #567).
//!
//! Records every terminal sent payment (from [`LightningEvent::PaymentEvent`],
//! with the preimage attached from the preceding [`LightningEvent::PaymentReceipt`])
//! and every claimed inbound payment (from [`LightningEvent::PaymentReceived`]).
//!
//! This is a best-effort, boot-scoped history: it is not persisted across
//! restarts and it is capped, so it is a debugging/audit surface, not an
//! accounting source of truth.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::model::response::PaymentHop;

use crate::LampoDaemon;

/// How many records to keep; the oldest are dropped first.
const HISTORY_CAP: usize = 1000;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PaymentRecord {
    /// "sent" or "received".
    pub direction: String,
    /// Hex encoded payment id (sent payments only).
    pub payment_id: Option<String>,
    /// Hex encoded payment hash, when known.
    pub payment_hash: Option<String>,
    /// "Success" for received payments; the terminal state otherwise.
    pub state: String,
    /// Amount in msat (received payments only; the sent-payment events do
    /// not carry the amount).
    pub amount_msat: Option<u64>,
    /// Hex encoded preimage, when known.
    pub preimage: Option<String>,
    /// Failure reason, for failed sent payments.
    pub reason: Option<String>,
    /// Hop path of a sent payment, when known.
    pub path: Vec<PaymentHop>,
    /// Unix seconds.
    pub timestamp: u64,
}

#[derive(Clone, Default)]
pub struct PaymentHistory {
    records: Arc<Mutex<Vec<PaymentRecord>>>,
}

impl PaymentHistory {
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&self, record: PaymentRecord) {
        let mut records = self.records.lock().unwrap();
        records.push(record);
        let len = records.len();
        if len > HISTORY_CAP {
            records.drain(0..len - HISTORY_CAP);
        }
    }

    pub fn list(&self) -> Vec<PaymentRecord> {
        self.records.lock().unwrap().clone()
    }
}

/// Subscribe to the event bus forever, recording payments into
/// [`LampoDaemon::payments`].
pub async fn record_payments(ctx: Arc<LampoDaemon>) -> ! {
    let mut events = ctx.handler().events();
    // PaymentReceipt arrives just before the terminal PaymentEvent; hold the
    // preimage here until the terminal event for the same payment id lands.
    let mut preimages: HashMap<String, String> = HashMap::new();
    loop {
        let Some(event) = events.recv().await else {
            // The event bus closed; nothing more will ever arrive. Spin
            // idly rather than busy-loop so the task stays parked.
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            continue;
        };
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        match event {
            Event::Lightning(LightningEvent::PaymentReceipt {
                payment_id,
                payment_preimage,
                ..
            }) => {
                preimages.insert(payment_id, payment_preimage);
            }
            Event::Lightning(LightningEvent::PaymentEvent {
                state,
                payment_id,
                payment_hash,
                path,
                reason,
            }) => {
                let preimage = payment_id
                    .as_ref()
                    .and_then(|id| preimages.remove(id.as_str()));
                ctx.payments().push(PaymentRecord {
                    direction: "sent".to_owned(),
                    payment_id,
                    payment_hash,
                    state: format!("{state:?}"),
                    amount_msat: None,
                    preimage,
                    reason,
                    path,
                    timestamp,
                });
            }
            Event::Lightning(LightningEvent::PaymentReceived {
                payment_hash,
                amount_msat,
                payment_preimage,
            }) => {
                ctx.payments().push(PaymentRecord {
                    direction: "received".to_owned(),
                    payment_id: None,
                    payment_hash: Some(payment_hash),
                    state: "Success".to_owned(),
                    amount_msat: Some(amount_msat),
                    preimage: payment_preimage,
                    reason: None,
                    path: Vec::new(),
                    timestamp,
                });
            }
            _ => {}
        }
    }
}
