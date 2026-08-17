//! Persistence backend interface.
//!
//! Lampo talks to its store through this trait rather than a concrete type, the
//! same way it talks to the chain through [`Backend`]. A backend has two faces:
//!
//! * LDK's [`KVStoreSync`], a supertrait, for state that is an opaque blob to
//!   everyone but LDK: channel monitors, the channel manager, the graph, the
//!   scorer, the BOLT 12 payer proofs. Key/value is the right shape there, and
//!   it is what every Lightning implementation uses for this data.
//! * [`PaymentStore`], for lampo's own records, which are *queried* rather than
//!   fetched by key ("every payment last year"). Key/value is the wrong shape
//!   for that, so SQL backends answer it with indexed columns, the way Core
//!   Lightning keeps its wallet tables.
//!
//! Backends must not acknowledge a write before it is durable: LDK broadcasts
//! the newest channel state it has persisted, so a write that is acked and then
//! lost can make the node force-close with a stale commitment.
//!
//! [`Backend`]: crate::backend::Backend
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::error;
use crate::json;
use crate::json::{Deserialize, Serialize};
use crate::ldk::io;
use crate::ldk::persister::fs_store::v1::FilesystemStore;
use crate::ldk::util::persist::KVStoreSync;

/// Namespace holding the payment records.
pub const PAYMENTS_NAMESPACE: &str = "payments";
/// Daemon-owned backend selection marker stored beside the filesystem backend,
/// but not part of its key/value state.
pub const STORAGE_SELECTION_FILE: &str = ".lampo-storage-backend";

/// Persistence backend kind supported by lampo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceKind {
    Filesystem,
    Sqlite,
    Postgres,
}

/// Which way the money went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaymentDirection {
    Inbound,
    Outbound,
}

/// Where a payment ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaymentStatus {
    Pending,
    Succeeded,
    Failed,
}

impl PaymentDirection {
    /// Stored form. SQL backends keep this in a column, so it has to survive a
    /// round trip unchanged.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }

    pub fn from_str(raw: &str) -> error::Result<Self> {
        match raw {
            "inbound" => Ok(Self::Inbound),
            "outbound" => Ok(Self::Outbound),
            _ => error::bail!("unknown payment direction `{raw}`"),
        }
    }
}

impl PaymentStatus {
    /// Stored form, see [`PaymentDirection::as_str`].
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    pub fn from_str(raw: &str) -> error::Result<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            _ => error::bail!("unknown payment status `{raw}`"),
        }
    }
}

/// A payment as lampo records it, flat so a SQL backend can keep one column per
/// field and index the ones that get filtered on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentRecord {
    /// Hex payment id for an outbound payment, hex payment hash for an inbound
    /// one, which is what each side is addressed by.
    pub id: String,
    pub payment_hash: String,
    pub direction: PaymentDirection,
    pub amount_msat: u64,
    /// Routing fee, known for outbound payments only.
    pub fee_msat: Option<u64>,
    pub status: PaymentStatus,
    /// Seconds since the Unix epoch.
    pub created_at: u64,
    /// The BOLT 11 invoice or BOLT 12 offer this paid, when there was one.
    pub invoice: Option<String>,
}

impl PaymentRecord {
    /// Key for a key/value backend.
    ///
    /// The timestamp comes first and is zero padded, so a store that can only
    /// list by prefix still answers "payments in this window" by scanning that
    /// window rather than every payment ever made.
    pub fn storage_key(&self) -> String {
        format!("{:020}-{}", self.created_at, self.id)
    }
}

/// Which payments to return. Every field unset means all of them.
#[derive(Debug, Clone, Default)]
pub struct PaymentFilter {
    pub from_unix_secs: Option<u64>,
    pub to_unix_secs: Option<u64>,
    pub direction: Option<PaymentDirection>,
    pub status: Option<PaymentStatus>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

impl PaymentFilter {
    /// Does `record` pass the field filters, ignoring limit and offset?
    ///
    /// SQL backends translate the filter into a query instead; this is for
    /// backends that have to walk the records themselves.
    pub fn matches(&self, record: &PaymentRecord) -> bool {
        if self
            .from_unix_secs
            .is_some_and(|from| record.created_at < from)
        {
            return false;
        }
        if self.to_unix_secs.is_some_and(|to| record.created_at > to) {
            return false;
        }
        if self
            .direction
            .is_some_and(|direction| record.direction != direction)
        {
            return false;
        }
        if self.status.is_some_and(|status| record.status != status) {
            return false;
        }
        true
    }

    /// Apply limit and offset to records already sorted oldest first.
    pub fn paginate(&self, records: Vec<PaymentRecord>) -> Vec<PaymentRecord> {
        let offset = self.offset.unwrap_or(0) as usize;
        let limit = self.limit.map_or(usize::MAX, |limit| limit as usize);
        records.into_iter().skip(offset).take(limit).collect()
    }
}

/// Lampo's own records, queried rather than fetched by key.
pub trait PaymentStore: Send + Sync {
    /// Record a payment, replacing any earlier record with the same id.
    fn upsert_payment(&self, payment: &PaymentRecord) -> error::Result<()>;

    fn get_payment(&self, id: &str) -> error::Result<Option<PaymentRecord>>;

    /// Payments matching `filter`, oldest first.
    fn list_payments(&self, filter: &PaymentFilter) -> error::Result<Vec<PaymentRecord>>;
}

/// Persistence backend specification.
///
/// Implementors inherit [`KVStoreSync`], whose `read` must report a missing key
/// as [`io::ErrorKind::NotFound`]: callers such as the BOLT 12 payer proof store
/// tell "no record" from "store broken" by that error kind alone.
///
/// [`io::ErrorKind::NotFound`]: crate::ldk::io::ErrorKind::NotFound
pub trait LampoPersistenceBackend: KVStoreSync + PaymentStore + Send + Sync {
    /// Return the kind of backend.
    fn kind(&self) -> PersistenceKind;

    /// Every `(primary_namespace, secondary_namespace, key)` in the store.
    ///
    /// The VSS shadow uses this to copy a node's existing state when it is
    /// enabled after the fact; a shadow that only mirrors future writes would
    /// silently miss any value that never changes again.
    fn list_all_keys(&self) -> error::Result<Vec<(String, String, String)>>;
}

/// Adapts lampo's synchronous backend interface to LDK's async [`KVStore`].
///
/// Database calls and filesystem fsyncs run on Tokio's blocking pool instead
/// of blocking the runtime thread that drives peer I/O.
#[derive(Clone)]
pub struct LampoAsyncPersistence(Arc<dyn LampoPersistenceBackend>);

impl LampoAsyncPersistence {
    pub fn new(store: Arc<dyn LampoPersistenceBackend>) -> Self {
        Self(store)
    }
}

impl crate::ldk::util::persist::KVStore for LampoAsyncPersistence {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, io::Error>> + 'static + Send {
        let store = self.0.clone();
        let (primary, secondary, key) = owned_key(primary_namespace, secondary_namespace, key);
        offload(move || store.read(&primary, &secondary, &key))
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<(), io::Error>> + 'static + Send {
        let store = self.0.clone();
        let (primary, secondary, key) = owned_key(primary_namespace, secondary_namespace, key);
        offload(move || store.write(&primary, &secondary, &key, buf))
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        lazy: bool,
    ) -> impl std::future::Future<Output = Result<(), io::Error>> + 'static + Send {
        let store = self.0.clone();
        let (primary, secondary, key) = owned_key(primary_namespace, secondary_namespace, key);
        offload(move || store.remove(&primary, &secondary, &key, lazy))
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> impl std::future::Future<Output = Result<Vec<String>, io::Error>> + 'static + Send {
        let store = self.0.clone();
        let (primary, secondary) = (primary_namespace.to_owned(), secondary_namespace.to_owned());
        offload(move || store.list(&primary, &secondary))
    }
}

fn owned_key(primary: &str, secondary: &str, key: &str) -> (String, String, String) {
    (primary.to_owned(), secondary.to_owned(), key.to_owned())
}

fn offload<T: Send + 'static>(
    call: impl FnOnce() -> Result<T, io::Error> + Send + 'static,
) -> impl std::future::Future<Output = Result<T, io::Error>> + 'static + Send {
    async move {
        tokio::task::spawn_blocking(call)
            .await
            .unwrap_or_else(|err| Err(io::Error::new(io::ErrorKind::Other, err)))
    }
}

/// The filesystem backend: LDK's store for the key/value half, and payments
/// held in memory and written through to the `payments` namespace.
///
/// Keeping every payment in memory is what ldk-node does, and it is fine at the
/// size a filesystem node reaches. It is also why the SQL backends exist: they
/// answer a query from an index instead of from a map that has to be loaded in
/// full at startup.
pub struct FsPersistence {
    inner: FilesystemStore,
    payments: Mutex<HashMap<String, PaymentRecord>>,
}

impl FsPersistence {
    /// Open the store under `data_dir`, seeding the payment map from disk.
    ///
    /// A record that fails to decode is skipped rather than fatal: it must not
    /// keep a node with live channels from starting.
    pub fn new(data_dir: PathBuf) -> Self {
        let inner = FilesystemStore::new(data_dir);
        let mut payments = HashMap::new();
        match KVStoreSync::list(&inner, PAYMENTS_NAMESPACE, "") {
            Ok(mut keys) => {
                // The key starts with the creation time, so sorting makes a
                // store left holding two records for one id (written by an
                // older lampo) resolve to the newer one, rather than to
                // whichever the directory happened to list first.
                keys.sort();
                for key in keys {
                    match KVStoreSync::read(&inner, PAYMENTS_NAMESPACE, "", &key)
                        .map_err(error::Error::from)
                        .and_then(|buf| Ok(json::from_slice::<PaymentRecord>(&buf)?))
                    {
                        Ok(record) => {
                            payments.insert(record.id.clone(), record);
                        }
                        Err(err) => {
                            log::error!(target: "lampo-persist", "skipping payment record `{key}`: {err}")
                        }
                    }
                }
            }
            Err(err) => {
                log::error!(target: "lampo-persist", "listing stored payments: {err}")
            }
        }
        Self {
            inner,
            payments: Mutex::new(payments),
        }
    }
}

impl KVStoreSync for FsPersistence {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, crate::ldk::io::Error> {
        KVStoreSync::read(&self.inner, primary_namespace, secondary_namespace, key)
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), crate::ldk::io::Error> {
        KVStoreSync::write(
            &self.inner,
            primary_namespace,
            secondary_namespace,
            key,
            buf,
        )
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        lazy: bool,
    ) -> Result<(), crate::ldk::io::Error> {
        KVStoreSync::remove(
            &self.inner,
            primary_namespace,
            secondary_namespace,
            key,
            lazy,
        )
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, crate::ldk::io::Error> {
        KVStoreSync::list(&self.inner, primary_namespace, secondary_namespace)
    }
}

impl PaymentStore for FsPersistence {
    fn upsert_payment(&self, payment: &PaymentRecord) -> error::Result<()> {
        let key = payment.storage_key();
        // Hold the map across disk mutation so concurrent upserts for one id
        // cannot leave memory and disk disagreeing, or retain two timestamped
        // files for the same payment.
        let mut payments = self
            .payments
            .lock()
            .map_err(|err| error::anyhow!("payment map poisoned: {err}"))?;
        let stale_key = payments
            .get(&payment.id)
            .map(|previous| previous.storage_key())
            .filter(|previous| previous != &key);

        // Disk first: a record held only in memory would not survive a restart,
        // and reporting success for it would be a lie.
        KVStoreSync::write(
            &self.inner,
            PAYMENTS_NAMESPACE,
            "",
            &key,
            json::to_vec(payment)?,
        )?;
        if let Some(stale_key) = stale_key {
            KVStoreSync::remove(&self.inner, PAYMENTS_NAMESPACE, "", &stale_key, false)?;
        }
        payments.insert(payment.id.clone(), payment.clone());
        Ok(())
    }

    fn get_payment(&self, id: &str) -> error::Result<Option<PaymentRecord>> {
        Ok(self
            .payments
            .lock()
            .map_err(|err| error::anyhow!("payment map poisoned: {err}"))?
            .get(id)
            .cloned())
    }

    fn list_payments(&self, filter: &PaymentFilter) -> error::Result<Vec<PaymentRecord>> {
        let mut matched: Vec<PaymentRecord> = self
            .payments
            .lock()
            .map_err(|err| error::anyhow!("payment map poisoned: {err}"))?
            .values()
            .filter(|record| filter.matches(record))
            .cloned()
            .collect();
        matched.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(filter.paginate(matched))
    }
}

impl LampoPersistenceBackend for FsPersistence {
    fn kind(&self) -> PersistenceKind {
        PersistenceKind::Filesystem
    }

    fn list_all_keys(&self) -> error::Result<Vec<(String, String, String)>> {
        use crate::ldk::util::persist::MigratableKVStoreSync;
        Ok(self
            .inner
            .list_all_keys()?
            .into_iter()
            .filter(|(primary, secondary, key)| {
                !(primary.is_empty() && secondary.is_empty() && key == STORAGE_SELECTION_FILE)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use crate::ldk::io::ErrorKind;

    use super::*;

    /// Scratch directory that removes itself when the test ends. Unique per
    /// test, so a parallel run does not share state.
    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            static NONCE: AtomicU32 = AtomicU32::new(0);
            let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "lampo-persist-{name}-{}-{nonce}",
                std::process::id()
            )))
        }

        fn backend(&self) -> Arc<dyn LampoPersistenceBackend> {
            Arc::new(FsPersistence::new(self.0.clone()))
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn payment(id: &str, created_at: u64, status: PaymentStatus) -> PaymentRecord {
        PaymentRecord {
            id: id.to_owned(),
            payment_hash: format!("hash-{id}"),
            direction: PaymentDirection::Outbound,
            amount_msat: 1_000,
            fee_msat: Some(1),
            status,
            created_at,
            invoice: None,
        }
    }

    #[test]
    fn filesystem_backend_reports_its_kind() {
        let dir = ScratchDir::new("kind");
        assert_eq!(dir.backend().kind(), PersistenceKind::Filesystem);
    }

    /// The trait object must round-trip through the LDK store methods, since
    /// that is how every LDK component reaches persistence.
    #[test]
    fn trait_object_round_trips_a_value() {
        let dir = ScratchDir::new("round-trip");
        let store = dir.backend();
        store.write("ns", "", "key", b"value".to_vec()).unwrap();

        assert_eq!(store.read("ns", "", "key").unwrap(), b"value");
        assert_eq!(store.list("ns", "").unwrap(), vec!["key".to_string()]);

        store.remove("ns", "", "key", false).unwrap();
        assert!(store.list("ns", "").unwrap().is_empty());
    }

    /// `payer_proof::load` maps this exact error kind to "no proof stored", so a
    /// backend reporting something else would turn a missing record into an error.
    #[test]
    fn reading_a_missing_key_reports_not_found() {
        let dir = ScratchDir::new("missing");
        let err = dir.backend().read("ns", "", "absent").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn payments_round_trip_and_upsert_replaces() {
        let dir = ScratchDir::new("payments");
        let store = dir.backend();

        store
            .upsert_payment(&payment("a", 100, PaymentStatus::Pending))
            .unwrap();
        assert_eq!(
            store.get_payment("a").unwrap().unwrap().status,
            PaymentStatus::Pending
        );

        store
            .upsert_payment(&payment("a", 100, PaymentStatus::Succeeded))
            .unwrap();
        assert_eq!(
            store.get_payment("a").unwrap().unwrap().status,
            PaymentStatus::Succeeded
        );
        assert_eq!(
            store
                .list_payments(&PaymentFilter::default())
                .unwrap()
                .len(),
            1
        );
        assert!(store.get_payment("absent").unwrap().is_none());
    }

    /// The point of the typed store: ask for a window and get only that window.
    #[test]
    fn list_payments_filters_by_time_status_and_page() {
        let dir = ScratchDir::new("filter");
        let store = dir.backend();
        for (id, at) in [("a", 100), ("b", 200), ("c", 300)] {
            store
                .upsert_payment(&payment(id, at, PaymentStatus::Succeeded))
                .unwrap();
        }
        store
            .upsert_payment(&payment("d", 250, PaymentStatus::Failed))
            .unwrap();

        let window = store
            .list_payments(&PaymentFilter {
                from_unix_secs: Some(150),
                to_unix_secs: Some(300),
                status: Some(PaymentStatus::Succeeded),
                ..Default::default()
            })
            .unwrap();
        let ids: Vec<&str> = window.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["b", "c"],
            "failed and out-of-window records excluded"
        );

        let page = store
            .list_payments(&PaymentFilter {
                limit: Some(2),
                offset: Some(1),
                ..Default::default()
            })
            .unwrap();
        let ids: Vec<&str> = page.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "d"], "oldest first, one skipped");
    }

    /// Payments have to come back after a restart, not just live in the map.
    #[test]
    fn payments_survive_reopening_the_store() {
        let dir = ScratchDir::new("reopen");
        dir.backend()
            .upsert_payment(&payment("a", 100, PaymentStatus::Succeeded))
            .unwrap();

        let reopened = dir.backend();
        assert_eq!(
            reopened.get_payment("a").unwrap().unwrap().status,
            PaymentStatus::Succeeded
        );
    }

    /// The key carries the creation time, so an upsert that moves it must
    /// replace the record rather than leave a second one behind for the same id.
    #[test]
    fn upserting_a_record_with_a_new_time_leaves_one_copy() {
        let dir = ScratchDir::new("retime");
        let store = dir.backend();
        store
            .upsert_payment(&payment("a", 100, PaymentStatus::Pending))
            .unwrap();
        store
            .upsert_payment(&payment("a", 200, PaymentStatus::Succeeded))
            .unwrap();

        assert_eq!(
            store
                .list_payments(&PaymentFilter::default())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store.list(PAYMENTS_NAMESPACE, "").unwrap().len(),
            1,
            "the record written under the old time should be gone"
        );

        // And the surviving one has to be the newer record after a restart.
        let reopened = dir.backend();
        let stored = reopened.get_payment("a").unwrap().unwrap();
        assert_eq!(stored.status, PaymentStatus::Succeeded);
        assert_eq!(stored.created_at, 200);
    }

    /// The key has to sort by time so a prefix-only store can scan a window.
    #[test]
    fn storage_key_sorts_by_creation_time() {
        let early = payment("z", 100, PaymentStatus::Succeeded).storage_key();
        let late = payment("a", 2_000, PaymentStatus::Succeeded).storage_key();
        assert!(early < late, "{early} should sort before {late}");
    }
}
