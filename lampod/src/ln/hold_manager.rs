//! Lampo Hold Manager.
//!
//! A hold payment (aka hodl invoice) is an incoming payment for an
//! invoice built on top of an external payment hash, so the preimage
//! is known only by the caller. When the payment arrives the node
//! cannot settle it, and the HTLCs are kept pending until the caller
//! claims the payment with the preimage or fails it back.
//!
//! The hold records are persisted inside the lampo persistence,
//! because LDK does not replay `PaymentClaimable` across restarts:
//! without a durable record a held payment would become unreachable
//! after a restart and silently expire.
//!
//! Author: Vincenzo Palazzo <vincenzopalazzo@member.fsf.org>
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use crate::persistence::LampoPersistence;
use lampo_common::bitcoin::hashes::sha256::Hash as Sha256;
use lampo_common::bitcoin::hashes::Hash;
use lampo_common::bitcoin::hex::FromHex;
use lampo_common::error;
use lampo_common::json;
use lampo_common::ldk::ln::channelmanager::FailureCode;
use lampo_common::ldk::types::payment::{PaymentHash, PaymentPreimage};
use lampo_common::ldk::util::persist::KVStoreSync;
use lampo_common::model::response::{Hold, HoldStatus};

use super::LampoChannelManager;

/// Namespace used inside the lampo persistence for the hold records.
const HOLD_PRIMARY_NAMESPACE: &str = "holds";
const HOLD_SECONDARY_NAMESPACE: &str = "";

/// What to do with an incoming claimable payment that we cannot
/// settle, decided by [`HoldManager::on_claimable`].
pub enum HoldDecision {
    /// The payment is registered, keep the HTLCs pending.
    Hold(Hold),
    /// The payment is registered but it pays less than the invoice
    /// asks for, so it must be failed back.
    Reject {
        expected_msat: u64,
        received_msat: u64,
    },
    /// A payment for this hash is already held: a second payment to
    /// the same hash must be failed back, as the spec requires.
    AlreadyHeld,
    /// We know nothing about this payment.
    NotRegistered,
}

/// Durable store for the hold records, kept separated from the
/// [`HoldManager`] so it can be tested without a running node.
pub(crate) struct HoldStore {
    persister: Arc<LampoPersistence>,
    holds: Mutex<HashMap<String, Hold>>,
}

impl HoldStore {
    pub(crate) fn new(persister: Arc<LampoPersistence>) -> error::Result<Self> {
        let mut holds = HashMap::new();
        let keys = persister
            .list(HOLD_PRIMARY_NAMESPACE, HOLD_SECONDARY_NAMESPACE)
            .map_err(|err| error::anyhow!("failed to list hold records: {err}"))?;
        for key in keys {
            let buf = persister
                .read(HOLD_PRIMARY_NAMESPACE, HOLD_SECONDARY_NAMESPACE, &key)
                .map_err(|err| error::anyhow!("failed to read hold record `{key}`: {err}"))?;
            let hold: Hold = json::from_slice(&buf)?;
            holds.insert(key, hold);
        }
        Ok(Self {
            persister,
            holds: Mutex::new(holds),
        })
    }

    fn persist(&self, hold: &Hold) -> error::Result<()> {
        let buf = json::to_vec(hold)?;
        self.persister
            .write(
                HOLD_PRIMARY_NAMESPACE,
                HOLD_SECONDARY_NAMESPACE,
                &hold.payment_hash,
                buf,
            )
            .map_err(|err| error::anyhow!("failed to persist hold record: {err}"))?;
        Ok(())
    }

    fn forget(&self, payment_hash: &str) -> error::Result<()> {
        self.persister
            .remove(
                HOLD_PRIMARY_NAMESPACE,
                HOLD_SECONDARY_NAMESPACE,
                payment_hash,
                false,
            )
            .map_err(|err| error::anyhow!("failed to remove hold record: {err}"))?;
        Ok(())
    }

    pub(crate) fn register(
        &self,
        payment_hash: &str,
        expected_amount_msat: Option<u64>,
    ) -> error::Result<Hold> {
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        let mut holds = self.holds.lock().unwrap();
        if holds.contains_key(payment_hash) {
            error::bail!("hold for payment hash `{payment_hash}` already exists");
        }
        let hold = Hold {
            payment_hash: payment_hash.to_owned(),
            status: HoldStatus::Open,
            expected_amount_msat,
            held_amount_msat: None,
            claim_deadline: None,
        };
        self.persist(&hold)?;
        holds.insert(payment_hash.to_owned(), hold.clone());
        Ok(hold)
    }

    pub(crate) fn on_claimable(
        &self,
        payment_hash: &str,
        amount_msat: u64,
        claim_deadline: Option<u32>,
    ) -> HoldDecision {
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        let mut holds = self.holds.lock().unwrap();
        let Some(hold) = holds.get_mut(payment_hash) else {
            return HoldDecision::NotRegistered;
        };
        if hold.status == HoldStatus::Held {
            return HoldDecision::AlreadyHeld;
        }
        if let Some(expected_msat) = hold.expected_amount_msat {
            if amount_msat < expected_msat {
                return HoldDecision::Reject {
                    expected_msat,
                    received_msat: amount_msat,
                };
            }
        }
        let previous = hold.clone();
        hold.status = HoldStatus::Held;
        hold.held_amount_msat = Some(amount_msat);
        hold.claim_deadline = claim_deadline;
        let hold = hold.clone();
        if let Err(err) = self.persist(&hold) {
            // If we cannot make the held state durable a restart would
            // strand the payment, so it is safer to fail it back now
            // and let the payer retry.
            log::error!(target: "lampo::hold", "failed to persist held state for `{payment_hash}`: {err}");
            *holds.get_mut(payment_hash).unwrap() = previous;
            return HoldDecision::NotRegistered;
        }
        HoldDecision::Hold(hold)
    }

    pub(crate) fn take(&self, payment_hash: &str) -> error::Result<Hold> {
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        let mut holds = self.holds.lock().unwrap();
        let Some(hold) = holds.remove(payment_hash) else {
            error::bail!("no hold found for payment hash `{payment_hash}`");
        };
        self.forget(payment_hash)?;
        Ok(hold)
    }

    pub(crate) fn get(&self, payment_hash: &str) -> Option<Hold> {
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        self.holds.lock().unwrap().get(payment_hash).cloned()
    }

    pub(crate) fn list(&self) -> Vec<Hold> {
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        self.holds.lock().unwrap().values().cloned().collect()
    }
}

pub struct HoldManager {
    channel_manager: Arc<LampoChannelManager>,
    store: HoldStore,
}

impl HoldManager {
    pub fn new(
        channel_manager: Arc<LampoChannelManager>,
        persister: Arc<LampoPersistence>,
    ) -> error::Result<Self> {
        let store = HoldStore::new(persister)?;
        Ok(Self {
            channel_manager,
            store,
        })
    }

    /// Register a payment hash to be held when the payment arrives.
    pub fn register(
        &self,
        payment_hash: &str,
        expected_amount_msat: Option<u64>,
    ) -> error::Result<Hold> {
        // sanity check that the hash is a valid 32 byte hex string
        let _ = Self::parse_hash(payment_hash)?;
        self.store.register(payment_hash, expected_amount_msat)
    }

    /// Remove the record for a payment hash without touching any HTLC.
    /// Used to roll back a registration when the invoice creation fails.
    pub fn unregister(&self, payment_hash: &str) -> error::Result<Hold> {
        self.store.take(payment_hash)
    }

    /// Decide what to do with an incoming claimable payment we cannot
    /// settle ourselves. Called from the LDK event handler, it must
    /// never block.
    pub fn on_claimable(
        &self,
        payment_hash: &str,
        amount_msat: u64,
        claim_deadline: Option<u32>,
    ) -> HoldDecision {
        self.store
            .on_claimable(payment_hash, amount_msat, claim_deadline)
    }

    /// Claim a held payment with its preimage.
    pub fn claim(&self, payment_preimage: &str) -> error::Result<Hold> {
        let preimage: [u8; 32] = Self::parse_bytes(payment_preimage)?;
        let payment_hash = Sha256::hash(&preimage).to_string();
        let Some(hold) = self.store.get(&payment_hash) else {
            error::bail!("no hold found for payment hash `{payment_hash}`");
        };
        if hold.status != HoldStatus::Held {
            error::bail!("hold `{payment_hash}` has no pending payment to claim");
        }
        if let Some(claim_deadline) = hold.claim_deadline {
            let height = self.channel_manager.manager().current_best_block().height;
            if height >= claim_deadline {
                error::bail!(
                    "claim deadline `{claim_deadline}` passed (current height `{height}`), the payment has been failed back"
                );
            }
        }
        self.channel_manager
            .manager()
            .claim_funds(PaymentPreimage(preimage));
        self.store.take(&payment_hash)
    }

    /// Fail a registered payment back to the sender and forget it.
    pub fn fail(&self, payment_hash: &str) -> error::Result<Hold> {
        let hash = Self::parse_hash(payment_hash)?;
        let hold = self.store.take(payment_hash)?;
        // Failing back HTLCs that are not pending is a harmless no-op.
        self.channel_manager
            .manager()
            .fail_htlc_backwards_with_reason(&hash, FailureCode::IncorrectOrUnknownPaymentDetails);
        Ok(hold)
    }

    pub fn list(&self) -> Vec<Hold> {
        self.store.list()
    }

    fn parse_bytes(hex_str: &str) -> error::Result<[u8; 32]> {
        let bytes = Vec::<u8>::from_hex(hex_str)
            .map_err(|err| error::anyhow!("invalid hex string: {err}"))?;
        bytes
            .try_into()
            .map_err(|_| error::anyhow!("expected 32 bytes"))
    }

    fn parse_hash(payment_hash: &str) -> error::Result<PaymentHash> {
        Ok(PaymentHash(Self::parse_bytes(payment_hash)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (HoldStore, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "lampo-hold-store-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        let _ = std::fs::remove_dir_all(&path);
        let persister = Arc::new(LampoPersistence::new(path.clone()));
        (HoldStore::new(persister).unwrap(), path)
    }

    fn rand_suffix() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64
    }

    #[test]
    fn hold_records_survive_a_reload() {
        let (store, path) = store();
        let hash = "11".repeat(32);
        store.register(&hash, Some(1_000)).unwrap();
        assert!(matches!(
            store.on_claimable(&hash, 1_000, Some(42)),
            HoldDecision::Hold(_)
        ));

        // simulate a restart by re-reading the store from disk
        let persister = Arc::new(LampoPersistence::new(path.clone()));
        let reloaded = HoldStore::new(persister).unwrap();
        let hold = reloaded.get(&hash).unwrap();
        assert_eq!(hold.status, HoldStatus::Held);
        assert_eq!(hold.held_amount_msat, Some(1_000));
        assert_eq!(hold.claim_deadline, Some(42));

        reloaded.take(&hash).unwrap();
        let persister = Arc::new(LampoPersistence::new(path.clone()));
        let reloaded = HoldStore::new(persister).unwrap();
        assert!(reloaded.get(&hash).is_none());
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn duplicate_payments_to_a_held_hash_are_rejected() {
        let (store, path) = store();
        let hash = "33".repeat(32);
        store.register(&hash, Some(1_000)).unwrap();
        assert!(matches!(
            store.on_claimable(&hash, 1_000, None),
            HoldDecision::Hold(_)
        ));
        // the spec requires failing back a second payment to the same hash
        assert!(matches!(
            store.on_claimable(&hash, 1_000, None),
            HoldDecision::AlreadyHeld
        ));
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn underpaying_holds_are_rejected() {
        let (store, path) = store();
        let hash = "22".repeat(32);
        store.register(&hash, Some(2_000)).unwrap();
        assert!(matches!(
            store.on_claimable(&hash, 1_000, None),
            HoldDecision::Reject { .. }
        ));
        // the record stays open so a correct payment can still arrive
        assert_eq!(store.get(&hash).unwrap().status, HoldStatus::Open);
        let _ = std::fs::remove_dir_all(&path);
    }
}
