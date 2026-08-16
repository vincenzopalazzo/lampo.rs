//! VSS shadow: a second copy of the node's state.
//!
//! This is not a backend of its own. It wraps whichever backend is primary and
//! mirrors every write to a [VSS] server. Any primary gets the shadow, and
//! nothing else in lampo knows the shadow is there.
//!
//! Lampo does not yet ship the reader/import command needed to restore this
//! copy into a fresh primary. Until that command can validate lag and require
//! explicit acknowledgement before importing channel monitors, `vss-url` is
//! an experimental write-only shadow rather than a complete backup workflow.
//!
//! # What the shadow promises, and what it does not
//!
//! The primary's acknowledgement stays the only acknowledgement. Mirroring
//! happens on another thread, and a VSS server that is slow or unreachable must
//! never hold up a channel operation, so a write is acked before the copy
//! exists.
//!
//! The copy therefore lags, and that matters: restoring channel state from a
//! monitor the shadow had not caught up on means broadcasting a stale
//! commitment, which loses money. The queue is kept in the primary store rather
//! than in memory so the backlog survives a restart, and [`VssShadow::lag`]
//! reports it, so recovery tooling can refuse or warn rather than guess.
//! Payment history restored from a lagging shadow is merely incomplete, which
//! is a different matter.
//!
//! [VSS]: https://github.com/lightningdevkit/vss-server
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use lampo_common::error;
use lampo_common::json;
use lampo_common::ldk::io;
use lampo_common::ldk::util::persist::KVStoreSync;
use lampo_common::persist::{
    LampoPersistenceBackend, PaymentFilter, PaymentRecord, PaymentStore, PersistenceKind,
    PAYMENTS_NAMESPACE,
};

pub mod queue;
mod vss;

pub use queue::ShadowLag;
pub use vss::VssSink;

/// Where mirrored values go.
///
/// The shadow is written against this rather than against VSS directly, so the
/// queueing and retry logic can be tested without a server, and so a different
/// remote could be dropped in.
pub trait ShadowSink: Send + Sync {
    /// Store `value` under `key`. Called from the mirror thread, so it may
    /// block.
    fn put(&self, key: &str, value: &[u8]) -> error::Result<()>;

    /// Forget `key` in the shadow. Called from the mirror thread, so it may
    /// block.
    ///
    /// A key that is already absent is success: the desired end state is that
    /// the key is gone.
    fn delete(&self, key: &str) -> error::Result<()>;
}

/// How long to wait before retrying a failed mirror, and the ceiling that
/// backoff climbs to.
const RETRY_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// How many jobs to read from the queue at a time. A backlog can be large and
/// every job carries the value it mirrors, so it is walked in batches.
const DRAIN_BATCH: usize = 64;

/// A backend with every write mirrored to a shadow copy.
pub struct VssShadow {
    primary: Arc<dyn LampoPersistenceBackend>,
    next_seq: AtomicU64,
    /// Held across the primary write and the queueing that follows it.
    ///
    /// Without it two threads writing the same key can reach the queue in the
    /// opposite order to the primary, and the shadow then keeps the older value
    /// while reporting itself caught up. For a channel monitor that is a stale
    /// commitment waiting to be restored, so the ordering is worth serialising
    /// writes for. Only nodes that asked for a shadow pay this.
    write_order: Mutex<()>,
    /// Writes that could not be queued at all, so the shadow will never learn
    /// about them. Reported through [`ShadowLag::dropped`].
    dropped: AtomicU64,
    /// Set when there is work to do, so the mirror thread sleeps rather than
    /// polling.
    wakeup: Arc<(Mutex<bool>, Condvar)>,
}

impl VssShadow {
    /// Wrap `primary`, mirroring its writes to `sink`.
    ///
    /// Any work left over from a previous run is picked up.
    pub fn wrap(
        primary: Arc<dyn LampoPersistenceBackend>,
        sink: Arc<dyn ShadowSink>,
    ) -> error::Result<Arc<Self>> {
        let next_seq = queue::next_seq(primary.as_ref())?;
        let shadow = Arc::new(Self {
            next_seq: AtomicU64::new(next_seq),
            dropped: AtomicU64::new(queue::read_dropped(primary.as_ref())?),
            primary,
            write_order: Mutex::new(()),
            wakeup: Arc::new((Mutex::new(false), Condvar::new())),
        });

        // Always queue the complete current state when attaching a shadow.
        // The primary may have run without VSS since the previous attachment,
        // or this may be a different, empty VSS destination; neither case can
        // be inferred from a completion marker stored only in the primary.
        // The marker is cleared only after every value is durable in the
        // queue, so a partial seed is retried on the next start.
        queue::write_reconcile_required(shadow.primary.as_ref(), true)?;
        shadow.seed()?;
        queue::write_dropped(shadow.primary.as_ref(), 0)?;
        shadow.dropped.store(0, Ordering::SeqCst);
        queue::write_reconcile_required(shadow.primary.as_ref(), false)?;

        let worker = Arc::clone(&shadow);
        thread::Builder::new()
            .name("lampo-vss-shadow".to_owned())
            .spawn(move || worker.mirror_loop(sink))?;

        // Anything queued before the last shutdown is still waiting.
        shadow.nudge();
        Ok(shadow)
    }

    /// How far behind the copy is.
    pub fn lag(&self) -> error::Result<ShadowLag> {
        queue::read_lag(self.primary.as_ref(), self.dropped.load(Ordering::SeqCst))
    }

    /// The backend underneath, for callers that must not go through the shadow.
    pub fn primary(&self) -> Arc<dyn LampoPersistenceBackend> {
        Arc::clone(&self.primary)
    }

    /// Queue a value for mirroring. Never fails the caller's write: the value
    /// is already durable in the primary, and a shadow that cannot keep up is
    /// reported through [`Self::lag`], not by failing channel operations.
    ///
    /// The caller must hold [`Self::write_order`] across the primary write and
    /// this, so the queue ends up in the same order the primary did.
    fn mirror(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        value: &[u8],
    ) -> bool {
        self.enqueue_job(primary_namespace, secondary_namespace, key, value, false)
    }

    /// Queue a deletion for mirroring. Same crash and ordering rules as
    /// [`Self::mirror`].
    fn mirror_remove(&self, primary_namespace: &str, secondary_namespace: &str, key: &str) -> bool {
        self.enqueue_job(primary_namespace, secondary_namespace, key, &[], true)
    }

    fn enqueue_job(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        value: &[u8],
        delete: bool,
    ) -> bool {
        let job = queue::MirrorJob {
            seq: self.next_seq.fetch_add(1, Ordering::SeqCst),
            primary_namespace: primary_namespace.to_owned(),
            secondary_namespace: secondary_namespace.to_owned(),
            key: key.to_owned(),
            value: value.to_vec(),
            delete,
        };
        if let Err(err) = queue::enqueue(self.primary.as_ref(), &job) {
            // The shadow will never learn about this write, and draining the
            // queue will not fix it, so say so rather than letting lag() report
            // a copy that looks complete.
            self.record_dropped();
            log::error!(target: "lampo-vss", "not queued for the shadow, {}: {err}", job.shadow_key());
            return false;
        }
        self.nudge();
        true
    }

    /// Queue everything already in the primary, so the copy starts from the
    /// node's full state rather than from whatever changes next.
    fn seed(&self) -> error::Result<()> {
        let _ordered = self.write_order.lock();
        for (primary_ns, secondary_ns, key) in self.primary.list_all_keys()? {
            if primary_ns == queue::QUEUE_NAMESPACE || primary_ns == queue::STATE_NAMESPACE {
                continue;
            }
            let value = self.primary.read(&primary_ns, &secondary_ns, &key)?;
            if !self.mirror(&primary_ns, &secondary_ns, &key, &value) {
                error::bail!("failed to queue {primary_ns}/{secondary_ns}/{key} while seeding");
            }
        }
        // SQL backends keep payments in a table rather than under a key, so
        // they are seeded through the typed store.
        if !matches!(self.primary.kind(), PersistenceKind::Filesystem) {
            for payment in self.primary.list_payments(&PaymentFilter::default())? {
                let encoded = json::to_vec(&payment)?;
                if !self.mirror(PAYMENTS_NAMESPACE, "", &payment.storage_key(), &encoded) {
                    error::bail!("failed to queue payment {} while seeding", payment.id);
                }
            }
        }
        Ok(())
    }

    /// Mark the primary as requiring reconciliation before mutating it.
    ///
    /// The returned value says whether an older write had already left the
    /// marker set; a later successful write must not clear that older hole.
    fn begin_primary_write(&self) -> error::Result<bool> {
        let already_required = queue::read_reconcile_required(self.primary.as_ref())?;
        queue::write_reconcile_required(self.primary.as_ref(), true)?;
        Ok(already_required)
    }

    /// Clear this write's marker after its queue entry is durable.
    fn finish_primary_write(&self, already_required: bool, queued: bool) {
        if !already_required && queued {
            if let Err(err) = queue::write_reconcile_required(self.primary.as_ref(), false) {
                // Leaving the marker set is conservative: lag() reports the
                // copy incomplete and the next start reconciles it.
                log::error!(target: "lampo-vss", "clearing the reconciliation marker: {err}");
            }
        }
    }

    /// Record a write the shadow will never learn about; see [`ShadowLag::dropped`].
    fn record_dropped(&self) {
        let dropped = self.dropped.fetch_add(1, Ordering::SeqCst) + 1;
        if let Err(err) = queue::write_dropped(self.primary.as_ref(), dropped) {
            // The in-memory count still reports it for this run.
            log::error!(target: "lampo-vss", "persisting the dropped count: {err}");
        }
    }

    /// Wake the mirror thread.
    fn nudge(&self) {
        let (lock, condvar) = &*self.wakeup;
        if let Ok(mut ready) = lock.lock() {
            *ready = true;
            condvar.notify_all();
        }
    }

    /// Drain the queue, forever, backing off while the sink is unhappy.
    fn mirror_loop(self: Arc<Self>, sink: Arc<dyn ShadowSink>) {
        let mut backoff = RETRY_BACKOFF;
        loop {
            let jobs = match queue::pending_batch(self.primary.as_ref(), DRAIN_BATCH) {
                Ok(jobs) => jobs,
                Err(err) => {
                    log::error!(target: "lampo-vss", "reading the shadow queue: {err}");
                    Vec::new()
                }
            };

            if jobs.is_empty() {
                self.wait_for_work(RETRY_BACKOFF);
                continue;
            }

            let mut stalled = false;
            for job in jobs {
                let result = if job.delete {
                    sink.delete(&job.shadow_key())
                } else {
                    sink.put(&job.shadow_key(), &job.value)
                };
                match result {
                    Ok(()) => {
                        backoff = RETRY_BACKOFF;
                        if let Err(err) = queue::dequeue(self.primary.as_ref(), job.seq) {
                            // The job is still queued, so carrying on would put
                            // it again immediately, forever, and would replay it
                            // over a newer value for the same key. Back off
                            // instead and let the primary recover.
                            log::error!(target: "lampo-vss", "clearing shadow job {}: {err}", job.seq);
                            stalled = true;
                            break;
                        }
                        if let Err(err) = queue::write_high_water(self.primary.as_ref(), job.seq) {
                            log::error!(target: "lampo-vss", "recording shadow progress: {err}");
                        }
                    }
                    Err(err) => {
                        // Stop at the first failure: the queue is ordered, and
                        // skipping ahead would mirror a newer value for a key
                        // whose older write has not landed.
                        log::warn!(
                            target: "lampo-vss",
                            "shadow write failed for {}, retrying in {}s: {err}",
                            job.shadow_key(),
                            backoff.as_secs()
                        );
                        stalled = true;
                        break;
                    }
                }
            }

            if stalled {
                self.wait_for_work(backoff);
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }

    /// Sleep until there is work or `timeout` passes.
    fn wait_for_work(&self, timeout: Duration) {
        let (lock, condvar) = &*self.wakeup;
        let Ok(mut ready) = lock.lock() else {
            thread::sleep(timeout);
            return;
        };
        // Take the flag before waiting rather than only after. A write that
        // arrived while the queue was being drained has already set it, and
        // waiting on the condvar at that point would miss the notification and
        // leave the job sitting until the timeout.
        if *ready {
            *ready = false;
            return;
        }
        if let Ok((mut ready, _)) = condvar.wait_timeout(ready, timeout) {
            *ready = false;
        }
    }
}

fn io_err(err: error::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, err.to_string())
}

impl KVStoreSync for VssShadow {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, io::Error> {
        self.primary
            .read(primary_namespace, secondary_namespace, key)
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), io::Error> {
        // The queue lives in the primary too, so do not mirror the mirroring.
        if primary_namespace == queue::QUEUE_NAMESPACE
            || primary_namespace == queue::STATE_NAMESPACE
        {
            return self
                .primary
                .write(primary_namespace, secondary_namespace, key, buf);
        }

        // Held across both, so the queue records writes in the order the
        // primary accepted them; see `write_order`.
        let _ordered = self.write_order.lock();
        let already_required = self.begin_primary_write().map_err(io_err)?;
        // The primary decides whether this write happened.
        self.primary
            .write(primary_namespace, secondary_namespace, key, buf.clone())?;
        let queued = self.mirror(primary_namespace, secondary_namespace, key, &buf);
        self.finish_primary_write(already_required, queued);
        Ok(())
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        lazy: bool,
    ) -> Result<(), io::Error> {
        // The queue lives in the primary too, so do not mirror the mirroring.
        if primary_namespace == queue::QUEUE_NAMESPACE
            || primary_namespace == queue::STATE_NAMESPACE
        {
            return self
                .primary
                .remove(primary_namespace, secondary_namespace, key, lazy);
        }

        // Removals must be mirrored: LDK archives a channel monitor by writing
        // it under archived_monitors and deleting the live key. Leaving the
        // live key in the shadow would restore a stale active monitor.
        let _ordered = self.write_order.lock();
        let already_required = self.begin_primary_write().map_err(io_err)?;
        self.primary
            .remove(primary_namespace, secondary_namespace, key, lazy)?;
        let queued = self.mirror_remove(primary_namespace, secondary_namespace, key);
        self.finish_primary_write(already_required, queued);
        Ok(())
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, io::Error> {
        self.primary.list(primary_namespace, secondary_namespace)
    }
}

impl PaymentStore for VssShadow {
    fn upsert_payment(&self, payment: &PaymentRecord) -> error::Result<()> {
        let _ordered = self.write_order.lock();
        let already_required = self.begin_primary_write()?;
        self.primary.upsert_payment(payment)?;
        // VSS is a key/value store, so the typed record travels as a blob and
        // is re-imported through this trait on recovery.
        let queued = match json::to_vec(payment) {
            Ok(encoded) => self.mirror(PAYMENTS_NAMESPACE, "", &payment.storage_key(), &encoded),
            Err(err) => {
                self.record_dropped();
                log::error!(target: "lampo-vss", "not queued for the shadow, payment {}: {err}", payment.id);
                false
            }
        };
        self.finish_primary_write(already_required, queued);
        Ok(())
    }

    fn get_payment(&self, id: &str) -> error::Result<Option<PaymentRecord>> {
        self.primary.get_payment(id)
    }

    fn list_payments(&self, filter: &PaymentFilter) -> error::Result<Vec<PaymentRecord>> {
        self.primary.list_payments(filter)
    }
}

impl LampoPersistenceBackend for VssShadow {
    /// The shadow is transparent: callers see the primary's kind.
    fn kind(&self) -> PersistenceKind {
        self.primary.kind()
    }

    fn list_all_keys(&self) -> error::Result<Vec<(String, String, String)>> {
        self.primary.list_all_keys()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;

    use lampo_common::persist::{FsPersistence, PaymentDirection, PaymentStatus};

    use super::*;

    /// Scratch directory that removes itself when the test ends.
    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            static NONCE: AtomicU64 = AtomicU64::new(0);
            let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
            Self(
                std::env::temp_dir()
                    .join(format!("lampo-vss-{name}-{}-{nonce}", std::process::id())),
            )
        }

        fn primary(&self) -> Arc<dyn LampoPersistenceBackend> {
            Arc::new(FsPersistence::new(self.0.clone()))
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A sink that records what it was given and can be told to fail.
    #[derive(Default)]
    struct FakeSink {
        stored: Mutex<HashMap<String, Vec<u8>>>,
        failing: AtomicBool,
    }

    impl FakeSink {
        fn stored(&self) -> HashMap<String, Vec<u8>> {
            self.stored.lock().unwrap().clone()
        }
    }

    impl ShadowSink for FakeSink {
        fn put(&self, key: &str, value: &[u8]) -> error::Result<()> {
            if self.failing.load(Ordering::SeqCst) {
                error::bail!("sink is down");
            }
            self.stored
                .lock()
                .unwrap()
                .insert(key.to_owned(), value.to_vec());
            Ok(())
        }

        fn delete(&self, key: &str) -> error::Result<()> {
            if self.failing.load(Ordering::SeqCst) {
                error::bail!("sink is down");
            }
            self.stored.lock().unwrap().remove(key);
            Ok(())
        }
    }

    /// A primary that can fail one read while an initial seed is in progress.
    struct FailingReadPrimary {
        inner: Arc<dyn LampoPersistenceBackend>,
        fail_key: String,
        failing: AtomicBool,
    }

    impl KVStoreSync for FailingReadPrimary {
        fn read(
            &self,
            primary_namespace: &str,
            secondary_namespace: &str,
            key: &str,
        ) -> Result<Vec<u8>, io::Error> {
            if key == self.fail_key && self.failing.load(Ordering::SeqCst) {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "injected seed read failure",
                ));
            }
            self.inner.read(primary_namespace, secondary_namespace, key)
        }

        fn write(
            &self,
            primary_namespace: &str,
            secondary_namespace: &str,
            key: &str,
            buf: Vec<u8>,
        ) -> Result<(), io::Error> {
            self.inner
                .write(primary_namespace, secondary_namespace, key, buf)
        }

        fn remove(
            &self,
            primary_namespace: &str,
            secondary_namespace: &str,
            key: &str,
            lazy: bool,
        ) -> Result<(), io::Error> {
            self.inner
                .remove(primary_namespace, secondary_namespace, key, lazy)
        }

        fn list(
            &self,
            primary_namespace: &str,
            secondary_namespace: &str,
        ) -> Result<Vec<String>, io::Error> {
            self.inner.list(primary_namespace, secondary_namespace)
        }
    }

    impl PaymentStore for FailingReadPrimary {
        fn upsert_payment(&self, payment: &PaymentRecord) -> error::Result<()> {
            self.inner.upsert_payment(payment)
        }

        fn get_payment(&self, id: &str) -> error::Result<Option<PaymentRecord>> {
            self.inner.get_payment(id)
        }

        fn list_payments(&self, filter: &PaymentFilter) -> error::Result<Vec<PaymentRecord>> {
            self.inner.list_payments(filter)
        }
    }

    impl LampoPersistenceBackend for FailingReadPrimary {
        fn kind(&self) -> PersistenceKind {
            self.inner.kind()
        }

        fn list_all_keys(&self) -> error::Result<Vec<(String, String, String)>> {
            let mut keys = self.inner.list_all_keys()?;
            keys.sort();
            Ok(keys)
        }
    }

    /// Wait for `check` to hold, so tests do not race the mirror thread.
    fn eventually(what: &str, check: impl Fn() -> bool) {
        for _ in 0..200 {
            if check() {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting for {what}");
    }

    fn payment(id: &str) -> PaymentRecord {
        PaymentRecord {
            id: id.to_owned(),
            payment_hash: format!("hash-{id}"),
            direction: PaymentDirection::Outbound,
            amount_msat: 1_000,
            fee_msat: Some(1),
            status: PaymentStatus::Succeeded,
            created_at: 100,
            invoice: None,
        }
    }

    #[test]
    fn writes_reach_the_primary_and_then_the_shadow() {
        let dir = ScratchDir::new("mirror");
        let sink = Arc::new(FakeSink::default());
        let shadow = VssShadow::wrap(dir.primary(), sink.clone()).unwrap();

        shadow.write("ns", "sub", "key", b"value".to_vec()).unwrap();

        // The primary is authoritative and answers immediately.
        assert_eq!(shadow.read("ns", "sub", "key").unwrap(), b"value");
        eventually("the value to reach the shadow", || {
            sink.stored().get("ns/sub/key").map(Vec::as_slice) == Some(b"value".as_slice())
        });
    }

    #[test]
    fn the_shadow_reports_the_primary_kind() {
        let dir = ScratchDir::new("kind");
        let shadow = VssShadow::wrap(dir.primary(), Arc::new(FakeSink::default())).unwrap();
        assert_eq!(shadow.kind(), PersistenceKind::Filesystem);
    }

    /// A shadow that is down must not stop the node from writing.
    #[test]
    fn a_failing_sink_does_not_fail_the_write() {
        let dir = ScratchDir::new("sink-down");
        let sink = Arc::new(FakeSink::default());
        sink.failing.store(true, Ordering::SeqCst);
        let shadow = VssShadow::wrap(dir.primary(), sink.clone()).unwrap();

        shadow.write("ns", "", "key", b"value".to_vec()).unwrap();
        assert_eq!(shadow.read("ns", "", "key").unwrap(), b"value");

        eventually("the backlog to be visible", || {
            shadow.lag().map(|lag| lag.pending == 1).unwrap_or(false)
        });
        assert!(
            sink.stored().is_empty(),
            "nothing should have been mirrored"
        );

        // Once the sink recovers the backlog drains on its own.
        sink.failing.store(false, Ordering::SeqCst);
        eventually("the backlog to drain", || {
            shadow.lag().map(|lag| lag.pending == 0).unwrap_or(false)
        });
        assert_eq!(
            sink.stored().get("ns//key").map(Vec::as_slice),
            Some(b"value".as_slice())
        );
    }

    /// The queue is in the primary, so a restart resumes rather than losing work.
    #[test]
    fn a_backlog_survives_a_restart() {
        let dir = ScratchDir::new("restart");
        let down = Arc::new(FakeSink::default());
        down.failing.store(true, Ordering::SeqCst);

        {
            let shadow = VssShadow::wrap(dir.primary(), down.clone()).unwrap();
            shadow.write("ns", "", "key", b"value".to_vec()).unwrap();
            eventually("the job to be queued", || {
                shadow.lag().map(|lag| lag.pending == 1).unwrap_or(false)
            });
        }

        // A new shadow over the same primary, with a working sink this time.
        let up = Arc::new(FakeSink::default());
        let shadow = VssShadow::wrap(dir.primary(), up.clone()).unwrap();
        eventually("the leftover job to be mirrored", || {
            up.stored().contains_key("ns//key")
        });
        assert_eq!(shadow.lag().unwrap().pending, 0);
    }

    #[test]
    fn payments_are_mirrored_as_blobs() {
        let dir = ScratchDir::new("payments");
        let sink = Arc::new(FakeSink::default());
        let shadow = VssShadow::wrap(dir.primary(), sink.clone()).unwrap();

        shadow.upsert_payment(&payment("a")).unwrap();

        assert!(shadow.get_payment("a").unwrap().is_some());
        eventually("the payment to reach the shadow", || {
            sink.stored()
                .keys()
                .any(|key| key.starts_with(PAYMENTS_NAMESPACE))
        });
    }

    /// The shadow must not end up holding an older value than the primary. Two
    /// threads writing one key concurrently is where that goes wrong: whoever
    /// wrote the primary last has to be whoever the shadow ends up with.
    #[test]
    fn concurrent_writes_to_one_key_reach_the_shadow_in_order() {
        let dir = ScratchDir::new("ordering");
        let sink = Arc::new(FakeSink::default());
        let shadow = VssShadow::wrap(dir.primary(), sink.clone()).unwrap();

        let writers: Vec<_> = (0..8)
            .map(|n| {
                let shadow = Arc::clone(&shadow);
                thread::spawn(move || {
                    shadow
                        .write("ns", "", "key", format!("value-{n}").into_bytes())
                        .unwrap();
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }

        let winner = shadow.read("ns", "", "key").unwrap();
        eventually("the shadow to catch up", || {
            shadow.lag().map(|lag| lag.pending == 0).unwrap_or(false)
        });
        assert_eq!(
            sink.stored().get("ns//key"),
            Some(&winner),
            "the shadow kept a different value than the primary"
        );
    }

    /// Enabling the shadow on a node with existing state must copy that state,
    /// not just what changes afterwards.
    #[test]
    fn wrapping_an_existing_store_seeds_the_shadow() {
        let dir = ScratchDir::new("seed");
        let primary = dir.primary();
        primary
            .write("ns", "", "old-key", b"old-value".to_vec())
            .unwrap();
        primary.upsert_payment(&payment("old-payment")).unwrap();

        let sink = Arc::new(FakeSink::default());
        let shadow = VssShadow::wrap(primary, sink.clone()).unwrap();
        eventually("the existing state to reach the shadow", || {
            shadow.lag().map(|lag| lag.pending == 0).unwrap_or(false)
                && sink.stored().contains_key("ns//old-key")
        });
        assert!(
            sink.stored()
                .keys()
                .any(|key| key.starts_with(PAYMENTS_NAMESPACE)),
            "the pre-existing payment was not seeded"
        );
    }

    /// LDK archives monitors by writing under archived_monitors and removing
    /// the live key. The shadow must forget that live key too.
    #[test]
    fn removals_are_mirrored_to_the_shadow() {
        let dir = ScratchDir::new("remove");
        let sink = Arc::new(FakeSink::default());
        let shadow = VssShadow::wrap(dir.primary(), sink.clone()).unwrap();

        shadow
            .write("monitors", "", "chan", b"live".to_vec())
            .unwrap();
        eventually("the live monitor to reach the shadow", || {
            sink.stored().contains_key("monitors//chan")
        });

        shadow.remove("monitors", "", "chan", false).unwrap();
        eventually("the live monitor to leave the shadow", || {
            shadow.lag().map(|lag| lag.pending == 0).unwrap_or(false)
                && !sink.stored().contains_key("monitors//chan")
        });
    }

    /// A previous shadow may be complete even though the primary was later
    /// used without VSS, or the configured destination may now be empty.
    #[test]
    fn reattaching_a_completed_shadow_reseeds_current_state() {
        let dir = ScratchDir::new("reseed");
        let primary = dir.primary();
        primary
            .write("ns", "", "key", b"current-value".to_vec())
            .unwrap();
        queue::write_reconcile_required(primary.as_ref(), false).unwrap();

        let sink = Arc::new(FakeSink::default());
        let shadow = VssShadow::wrap(primary, sink.clone()).unwrap();
        eventually("the reattached shadow to receive current state", || {
            shadow.lag().map(|lag| lag.pending == 0).unwrap_or(false)
                && sink.stored().get("ns//key").map(Vec::as_slice)
                    == Some(b"current-value".as_slice())
        });
    }

    /// A process killed after committing the primary but before enqueueing the
    /// value leaves the marker set. Startup must reconcile that value rather
    /// than report an empty queue as a complete shadow.
    #[test]
    fn interrupted_primary_write_is_reconciled_on_restart() {
        let dir = ScratchDir::new("interrupted-write");
        let primary = dir.primary();
        primary
            .write("ns", "", "key", b"old-value".to_vec())
            .unwrap();
        queue::write_reconcile_required(primary.as_ref(), false).unwrap();

        let sink = Arc::new(FakeSink::default());
        sink.stored
            .lock()
            .unwrap()
            .insert("ns//key".to_owned(), b"old-value".to_vec());

        // These are the two durable operations completed before the simulated
        // crash. There is deliberately no queue entry for the new value.
        queue::write_reconcile_required(primary.as_ref(), true).unwrap();
        primary
            .write("ns", "", "key", b"new-value".to_vec())
            .unwrap();

        let shadow = VssShadow::wrap(primary, sink.clone()).unwrap();
        eventually("the interrupted value to be reconciled", || {
            shadow
                .lag()
                .map(|lag| lag.pending == 0 && lag.dropped == 0)
                .unwrap_or(false)
                && sink.stored().get("ns//key").map(Vec::as_slice) == Some(b"new-value".as_slice())
        });
    }

    /// A failed seed must leave a durable incomplete marker, even after some
    /// values have already entered the queue, so the next start retries all of
    /// the primary state.
    #[test]
    fn a_partial_seed_is_retried_on_restart() {
        let dir = ScratchDir::new("partial-seed");
        let primary = dir.primary();
        primary.write("ns", "", "a", b"first".to_vec()).unwrap();
        primary.write("ns", "", "b", b"second".to_vec()).unwrap();
        let primary = Arc::new(FailingReadPrimary {
            inner: primary,
            fail_key: "b".to_owned(),
            failing: AtomicBool::new(true),
        });
        let sink = Arc::new(FakeSink::default());

        let first = VssShadow::wrap(primary.clone(), sink.clone());
        assert!(
            first.is_err(),
            "the injected read failure must abort seeding"
        );
        assert!(queue::read_reconcile_required(primary.as_ref()).unwrap());
        assert_eq!(queue::pending_keys(primary.as_ref()).unwrap().len(), 1);

        primary.failing.store(false, Ordering::SeqCst);
        let shadow = VssShadow::wrap(primary, sink.clone()).unwrap();
        eventually("the complete seed to be retried", || {
            shadow
                .lag()
                .map(|lag| lag.pending == 0 && lag.dropped == 0)
                .unwrap_or(false)
                && sink.stored().get("ns//a").map(Vec::as_slice) == Some(b"first".as_slice())
                && sink.stored().get("ns//b").map(Vec::as_slice) == Some(b"second".as_slice())
        });
    }

    /// An unreadable queue entry may be a transient store error or corruption,
    /// but removing it would let lag() call a stale shadow complete.
    #[test]
    fn an_unreadable_job_stays_pending() {
        let dir = ScratchDir::new("unreadable-job");
        let primary = dir.primary();
        let key = queue::MirrorJob::queue_key(1);
        primary
            .write(queue::QUEUE_NAMESPACE, "", &key, b"not-json".to_vec())
            .unwrap();

        assert!(queue::pending_batch(primary.as_ref(), 1).is_err());
        assert_eq!(queue::pending_keys(primary.as_ref()).unwrap(), vec![key]);
    }

    /// The queue is stored in the primary; mirroring it would never terminate.
    #[test]
    fn the_queue_is_not_mirrored_to_itself() {
        let dir = ScratchDir::new("no-recursion");
        let sink = Arc::new(FakeSink::default());
        let shadow = VssShadow::wrap(dir.primary(), sink.clone()).unwrap();

        shadow.write("ns", "", "key", b"value".to_vec()).unwrap();
        eventually("the value to reach the shadow", || {
            sink.stored().contains_key("ns//key")
        });

        let mirrored_bookkeeping = sink
            .stored()
            .keys()
            .filter(|key| {
                key.starts_with(queue::QUEUE_NAMESPACE) || key.starts_with(queue::STATE_NAMESPACE)
            })
            .count();
        assert_eq!(mirrored_bookkeeping, 0);
    }
}
