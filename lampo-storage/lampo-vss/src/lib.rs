//! VSS shadow: a second copy of the node's state, kept for recovery.
//!
//! This is not a backend of its own. It wraps whichever backend is primary and
//! mirrors every write to a [VSS] server, so a node that loses its database can
//! be rebuilt from the copy. Any primary gets the shadow, and nothing else in
//! lampo knows the shadow is there.
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
        let shadow = Arc::new(Self {
            next_seq: AtomicU64::new(queue::next_seq(primary.as_ref())?),
            primary,
            write_order: Mutex::new(()),
            dropped: AtomicU64::new(0),
            wakeup: Arc::new((Mutex::new(false), Condvar::new())),
        });

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
    fn mirror(&self, primary_namespace: &str, secondary_namespace: &str, key: &str, value: &[u8]) {
        let job = queue::MirrorJob {
            seq: self.next_seq.fetch_add(1, Ordering::SeqCst),
            primary_namespace: primary_namespace.to_owned(),
            secondary_namespace: secondary_namespace.to_owned(),
            key: key.to_owned(),
            value: value.to_vec(),
        };
        if let Err(err) = queue::enqueue(self.primary.as_ref(), &job) {
            // The shadow will never learn about this write, and draining the
            // queue will not fix it, so say so rather than letting lag() report
            // a copy that looks complete.
            self.dropped.fetch_add(1, Ordering::SeqCst);
            log::error!(target: "lampo-vss", "not queued for the shadow, {}: {err}", job.shadow_key());
            return;
        }
        self.nudge();
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
                match sink.put(&job.shadow_key(), &job.value) {
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
        // The primary decides whether this write happened.
        self.primary
            .write(primary_namespace, secondary_namespace, key, buf.clone())?;
        self.mirror(primary_namespace, secondary_namespace, key, &buf);
        Ok(())
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        lazy: bool,
    ) -> Result<(), io::Error> {
        // Removals are not mirrored: the shadow exists to rebuild a node, and
        // an extra stale key there is harmless next to a missing one.
        self.primary
            .remove(primary_namespace, secondary_namespace, key, lazy)
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
        self.primary.upsert_payment(payment)?;
        // VSS is a key/value store, so the typed record travels as a blob and
        // is re-imported through this trait on recovery.
        match json::to_vec(payment) {
            Ok(encoded) => self.mirror(PAYMENTS_NAMESPACE, "", &payment.storage_key(), &encoded),
            Err(err) => {
                self.dropped.fetch_add(1, Ordering::SeqCst);
                log::error!(target: "lampo-vss", "not queued for the shadow, payment {}: {err}", payment.id)
            }
        }
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
