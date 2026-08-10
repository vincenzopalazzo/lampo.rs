//! Swap model: one record per swap, keyed by payment hash, with the
//! state machine encoded as an enum so illegal transitions do not
//! typecheck into silence.
use serde::{Deserialize, Serialize};

use lampo_common::error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// A Spark user pays a BOLT12 offer: Spark HTLC in, lightning out.
    SparkToLn,
    /// A lightning payer funds a Spark address through one of our
    /// BOLT12 offers: lightning in, Spark HTLC out.
    LnToSpark,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    // --- Direction A: Spark -> LN ---
    /// The BOLT12 invoice has been fetched, we returned a quote and
    /// wait for the counterparty to lock the Spark HTLC on the hash.
    Quoted,
    /// The Spark HTLC is locked to us; the lightning payment is in
    /// flight through `payfetched`.
    LnPaying,
    /// The lightning payment settled and gave us the preimage; we are
    /// claiming the Spark HTLC with it.
    Claiming,

    // --- Direction B: LN -> Spark ---
    /// A hold invoice on the counterparty's payment hash has been
    /// issued. We cannot settle it: only they know the preimage.
    HoldInvoiceIssued,
    /// Their payment arrived and its htlcs are held, not settled. We
    /// owe them a Spark HTLC on the same hash before we can take it.
    LnHeld,
    /// The Spark HTLC is locked to them. When they claim it they
    /// reveal the preimage, which is what lets us settle the held
    /// lightning payment. Until then neither side has moved.
    SparkHtlcLocked,

    // --- Terminal ---
    Done,
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Swap {
    /// Hex payment hash; also the record id. Empty until known for
    /// Direction B (the hash only exists once a payer shows up).
    pub payment_hash: Option<String>,
    /// Direction B swaps are found by offer id before the hash exists.
    pub offer_id: Option<String>,
    pub direction: Direction,
    pub state: State,
    /// msat on the lightning leg: what we pay (A) or receive (B).
    pub amount_msat: u64,
    /// sats on the spark leg: what the counterparty locks and we claim
    /// (A) or what we deliver (B). The gap between this and the
    /// lightning leg, minus routing, is our fee.
    pub spark_amount_sat: u64,
    /// Direction A: the payment id handle for `payfetched`.
    pub lampo_payment_id: Option<String>,
    /// Direction B: the Spark transfer we owe, chosen *before* the
    /// transfer is created so a retry after a crash reuses the same id
    /// instead of paying a second time.
    pub spark_transfer_id: Option<String>,
    /// Direction A: the preimage the lightning leg revealed, persisted
    /// before we try to claim the counterparty's Spark HTLC so a crash
    /// mid-claim can retry instead of losing it.
    pub preimage: Option<String>,
    /// Direction B: where the Spark HTLC goes. Direction A: unused.
    pub counterparty_spark_address: Option<String>,
    /// The BOLT12 offer string (theirs in A, ours in B).
    pub offer: String,
    /// Unix seconds.
    pub created_at: u64,
    pub updated_at: u64,
}

impl Swap {
    pub fn id(&self) -> String {
        self.payment_hash
            .clone()
            .or_else(|| self.offer_id.clone())
            .unwrap_or_else(|| "unknown".to_owned())
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self.state, State::Done | State::Failed { .. })
    }

    /// Move to `next`, enforcing the state machine. Every transition
    /// must be persisted by the caller before acting on it.
    pub fn transition(&mut self, next: State) -> error::Result<()> {
        use State::*;
        let ok = matches!(
            (&self.state, &next),
            (Quoted, LnPaying)
                | (Quoted, Failed { .. })
                | (LnPaying, Claiming)
                | (LnPaying, Failed { .. })
                | (Claiming, Done)
                | (Claiming, Failed { .. })
                | (HoldInvoiceIssued, LnHeld)
                | (HoldInvoiceIssued, Failed { .. })
                | (LnHeld, SparkHtlcLocked)
                | (LnHeld, Failed { .. })
                | (SparkHtlcLocked, Done)
                | (SparkHtlcLocked, Failed { .. })
        );
        if !ok {
            error::bail!(
                "illegal swap transition `{:?}` -> `{:?}` for `{}`",
                self.state,
                next,
                self.id()
            );
        }
        self.state = next;
        self.updated_at = now();
        Ok(())
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swap(direction: Direction, state: State) -> Swap {
        Swap {
            payment_hash: Some("aa".repeat(32)),
            offer_id: None,
            direction,
            state,
            amount_msat: 1_000,
            spark_amount_sat: 1,
            lampo_payment_id: None,
            spark_transfer_id: None,
            preimage: None,
            counterparty_spark_address: None,
            offer: "lno1...".to_owned(),
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn direction_a_happy_path() {
        let mut s = swap(Direction::SparkToLn, State::Quoted);
        s.transition(State::LnPaying).unwrap();
        s.transition(State::Claiming).unwrap();
        s.transition(State::Done).unwrap();
        assert!(s.is_terminal());
    }

    #[test]
    fn direction_b_happy_path() {
        let mut s = swap(Direction::LnToSpark, State::HoldInvoiceIssued);
        s.transition(State::LnHeld).unwrap();
        s.transition(State::SparkHtlcLocked).unwrap();
        s.transition(State::Done).unwrap();
    }

    #[test]
    fn skipping_states_is_rejected() {
        let mut s = swap(Direction::SparkToLn, State::Quoted);
        assert!(s.transition(State::Done).is_err());
        let mut s = swap(Direction::LnToSpark, State::HoldInvoiceIssued);
        assert!(s.transition(State::SparkHtlcLocked).is_err());
    }

    #[test]
    fn terminal_states_are_final() {
        let mut s = swap(Direction::SparkToLn, State::Done);
        assert!(s.transition(State::Claiming).is_err());
        let mut s = swap(
            Direction::SparkToLn,
            State::Failed {
                reason: "x".to_owned(),
            },
        );
        assert!(s.transition(State::Quoted).is_err());
    }

    #[test]
    fn every_active_state_can_fail() {
        for state in [
            State::Quoted,
            State::LnPaying,
            State::Claiming,
            State::HoldInvoiceIssued,
            State::LnHeld,
            State::SparkHtlcLocked,
        ] {
            let direction = match state {
                State::Quoted | State::LnPaying | State::Claiming => Direction::SparkToLn,
                _ => Direction::LnToSpark,
            };
            let mut s = swap(direction, state);
            s.transition(State::Failed {
                reason: "boom".to_owned(),
            })
            .unwrap();
        }
    }
}
