//! Liquidity purchases this node owes the Phoenix LSP a funding fee for.
//!
//! Kept in the node's persister under `phoenix_lsp/purchases/<funding txid>`,
//! one JSON record per funding transaction, next to LDK's own data. The
//! claim path looks a purchase up by payment hash to decide whether a
//! skimmed funding fee is legitimate.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use lampo_common::bitcoin::hashes::sha256::Hash as Sha256;
use lampo_common::bitcoin::hashes::Hash;
use lampo_common::error;
use lampo_common::hex;
use lampo_common::json;
use lampo_common::ldk::io::ErrorKind;
use lampo_common::ldk::types::payment::PaymentHash;
use lampo_common::ldk::util::persist::KVStoreSync;
pub use lampo_common::model::response::PhoenixLspPurchase as Purchase;

use super::liquidity_ads::PaymentType;

const PRIMARY_NAMESPACE: &str = "phoenix_lsp";
const SECONDARY_NAMESPACE: &str = "purchases";

/// The node's key-value persister, shared with LDK.
pub type Store = Arc<dyn KVStoreSync + Send + Sync>;

/// Unix seconds now, for `created_at`.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Whether `purchase` was paid for with `payment_hash`: by hash for payment
/// types 128 and 130, by the preimage that hashes to it for type 129.
pub fn covers_payment_hash(purchase: &Purchase, payment_hash: &PaymentHash) -> bool {
    match PaymentType::from_bit(purchase.payment_type) {
        PaymentType::FromFutureHtlcWithPreimage => purchase.payment_hashes.iter().any(|preimage| {
            hex::decode(preimage)
                .map(|bytes| Sha256::hash(&bytes).to_byte_array() == payment_hash.0)
                .unwrap_or(false)
        }),
        _ => {
            let wanted = hex::encode(payment_hash.0);
            purchase
                .payment_hashes
                .iter()
                .any(|hash| hash.eq_ignore_ascii_case(&wanted))
        }
    }
}

/// The most the LSP may still skim from HTLCs for `purchase`: the fees it
/// quoted, minus what fee credit already covered.
pub fn max_funding_fee_msat(purchase: &Purchase) -> u64 {
    purchase
        .mining_fee_sat
        .saturating_add(purchase.service_fee_sat)
        .saturating_mul(1000)
        .saturating_sub(purchase.fee_credit_used_msat)
}

pub struct PurchaseStore {
    store: Store,
    inner: Mutex<Vec<Purchase>>,
}

impl PurchaseStore {
    /// Load every purchase from `store`.
    pub fn open(store: Store) -> error::Result<Self> {
        let keys = match store.list(PRIMARY_NAMESPACE, SECONDARY_NAMESPACE) {
            Ok(keys) => keys,
            Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
            Err(err) => return Err(err.into()),
        };
        let mut purchases = Vec::with_capacity(keys.len());
        for key in keys {
            let raw = store.read(PRIMARY_NAMESPACE, SECONDARY_NAMESPACE, &key)?;
            let purchase: Purchase = json::from_slice(&raw)
                .map_err(|err| error::anyhow!("purchase `{key}` cannot be decoded: {err}"))?;
            purchases.push(purchase);
        }
        Ok(Self {
            store,
            inner: Mutex::new(purchases),
        })
    }

    pub fn list(&self) -> Vec<Purchase> {
        self.lock().clone()
    }

    /// Insert `purchase`, replacing any record with the same funding txid.
    /// The txid is the key, so it must be 32 hex bytes; it is stored
    /// lowercased.
    pub fn upsert(&self, mut purchase: Purchase) -> error::Result<()> {
        let key = purchase.funding_txid.trim().to_ascii_lowercase();
        if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            error::bail!(
                "invalid funding txid `{}`: expected 32 hex bytes",
                purchase.funding_txid
            );
        }
        purchase.funding_txid = key.clone();
        let raw = json::to_vec(&purchase)?;
        let mut purchases = self.lock();
        self.store
            .write(PRIMARY_NAMESPACE, SECONDARY_NAMESPACE, &key, raw)?;
        match purchases
            .iter_mut()
            .find(|existing| existing.funding_txid == key)
        {
            Some(existing) => *existing = purchase,
            None => purchases.push(purchase),
        }
        Ok(())
    }

    pub fn find_by_payment_hash(&self, payment_hash: &PaymentHash) -> Option<Purchase> {
        self.lock()
            .iter()
            .find(|purchase| covers_payment_hash(purchase, payment_hash))
            .cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Purchase>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn purchase(payment_type: u32, payment_hashes: Vec<String>) -> Purchase {
        Purchase {
            funding_txid: "ab".repeat(32),
            amount_sat: 100_000,
            mining_fee_sat: 1_000,
            service_fee_sat: 2_500,
            payment_type,
            payment_hashes,
            fee_credit_used_msat: 500_000,
            created_at: 1,
        }
    }

    #[test]
    fn matches_by_hash_or_by_preimage() {
        let preimage = [0x42u8; 32];
        let hash = PaymentHash(Sha256::hash(&preimage).to_byte_array());
        let other = PaymentHash([0x01; 32]);

        let by_hash = purchase(128, vec![hex::encode(hash.0).to_uppercase()]);
        assert!(covers_payment_hash(&by_hash, &hash));
        assert!(!covers_payment_hash(&by_hash, &other));

        let by_preimage = purchase(129, vec![hex::encode(preimage)]);
        assert!(covers_payment_hash(&by_preimage, &hash));
        assert!(!covers_payment_hash(&by_preimage, &other));

        // Type 129 lists preimages: the hash itself must not match.
        let wrong_kind = purchase(129, vec![hex::encode(hash.0)]);
        assert!(!covers_payment_hash(&wrong_kind, &hash));

        assert_eq!(max_funding_fee_msat(&by_hash), 3_500_000 - 500_000);
    }

    /// A fresh directory under the system temp dir; removed by the caller.
    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "lampo-phoenix-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn store_round_trips_through_the_persister() {
        use lampo_common::ldk::persister::fs_store::v1::FilesystemStore;

        let dir = scratch_dir("purchases");
        let persister: Store = Arc::new(FilesystemStore::new(dir.clone()));
        let hash = PaymentHash([0x07; 32]);
        {
            let store = PurchaseStore::open(persister.clone()).unwrap();
            assert!(store.list().is_empty());
            store
                .upsert(purchase(128, vec![hex::encode(hash.0)]))
                .unwrap();
            let mut updated = purchase(130, vec![hex::encode(hash.0)]);
            updated.amount_sat = 200_000;
            updated.funding_txid = "AB".repeat(32);
            store.upsert(updated).unwrap();
            assert_eq!(store.list().len(), 1, "same txid replaces the record");
            assert_eq!(store.list()[0].amount_sat, 200_000);
            assert_eq!(store.list()[0].funding_txid, "ab".repeat(32));

            let mut bad = purchase(128, Vec::new());
            bad.funding_txid = "not a txid".to_owned();
            assert!(store.upsert(bad).is_err());
        }
        let store = PurchaseStore::open(persister.clone()).unwrap();
        let found = store.find_by_payment_hash(&hash).unwrap();
        assert_eq!(found.payment_type, 130);
        assert!(store
            .find_by_payment_hash(&PaymentHash([0x08; 32]))
            .is_none());
        assert_eq!(
            persister.list("phoenix_lsp", "purchases").unwrap(),
            vec!["ab".repeat(32)]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
