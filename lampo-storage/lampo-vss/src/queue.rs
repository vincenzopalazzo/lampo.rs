//! The shadow's pending work, kept in the primary store.
//!
//! A queue held only in memory would lose whatever had not been mirrored when
//! the node stopped, and the operator would have no way of knowing. Keeping it
//! in the primary store means the backlog survives a restart and can be
//! reported.
use lampo_common::error;
use lampo_common::json;
use lampo_common::json::{Deserialize, Serialize};
use lampo_common::persist::LampoPersistenceBackend;

/// Namespace holding jobs that have not reached the shadow yet.
pub const QUEUE_NAMESPACE: &str = "vss_shadow_queue";
/// Namespace holding how far the shadow has got.
pub const STATE_NAMESPACE: &str = "vss_shadow_state";
/// Key under [`STATE_NAMESPACE`] holding the high-water mark.
pub const HIGH_WATER_KEY: &str = "high_water";
/// Key under [`STATE_NAMESPACE`] holding the count of writes that never made
/// it into the queue. Persisted, because a restart must not launder a hole in
/// the copy.
pub const DROPPED_KEY: &str = "dropped";

/// One value waiting to be mirrored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorJob {
    /// Position in the queue. Jobs are drained in this order.
    pub seq: u64,
    pub primary_namespace: String,
    pub secondary_namespace: String,
    pub key: String,
    pub value: Vec<u8>,
}

impl MirrorJob {
    /// Queue key, zero padded so listing sorts oldest first.
    pub fn queue_key(seq: u64) -> String {
        format!("{seq:020}")
    }

    /// The key this value has in the shadow, flattened since VSS is one
    /// namespace of its own.
    pub fn shadow_key(&self) -> String {
        format!(
            "{}/{}/{}",
            self.primary_namespace, self.secondary_namespace, self.key
        )
    }
}

/// How far behind the shadow is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ShadowLag {
    /// Sequence number of the newest job known to have reached the shadow.
    pub mirrored_seq: u64,
    /// Jobs still waiting.
    pub pending: u64,
    /// Writes that could not even be queued, so the shadow does not know they
    /// exist and never will.
    ///
    /// Any value above zero means the copy has holes that draining the queue
    /// will not fill, and recovery from it cannot be trusted.
    pub dropped: u64,
}

/// Record `job` as waiting to be mirrored.
pub fn enqueue(primary: &dyn LampoPersistenceBackend, job: &MirrorJob) -> error::Result<()> {
    primary.write(
        QUEUE_NAMESPACE,
        "",
        &MirrorJob::queue_key(job.seq),
        json::to_vec(job)?,
    )?;
    Ok(())
}

/// Forget a job that has reached the shadow.
pub fn dequeue(primary: &dyn LampoPersistenceBackend, seq: u64) -> error::Result<()> {
    primary.remove(QUEUE_NAMESPACE, "", &MirrorJob::queue_key(seq), false)?;
    Ok(())
}

/// Keys of the jobs still waiting, oldest first.
///
/// The key is the zero padded sequence number, and the padding is as wide as
/// `u64::MAX`, so sorting them as text sorts them numerically.
pub fn pending_keys(primary: &dyn LampoPersistenceBackend) -> error::Result<Vec<String>> {
    let mut keys = primary.list(QUEUE_NAMESPACE, "")?;
    keys.sort();
    Ok(keys)
}

/// The next `limit` jobs, oldest first.
///
/// Only this many are read: a backlog built up while the shadow was
/// unreachable can be large, and every job carries the value it is mirroring,
/// so reading all of them to make progress on the first would mean holding the
/// whole backlog in memory.
///
/// A job that cannot be decoded is dropped rather than blocking the queue
/// forever; it is logged, and the value it carried will be re-mirrored the next
/// time that key is written.
pub fn pending_batch(
    primary: &dyn LampoPersistenceBackend,
    limit: usize,
) -> error::Result<Vec<MirrorJob>> {
    let mut jobs = Vec::new();
    for key in pending_keys(primary)?.into_iter().take(limit) {
        match primary
            .read(QUEUE_NAMESPACE, "", &key)
            .map_err(error::Error::from)
            .and_then(|buf| Ok(json::from_slice::<MirrorJob>(&buf)?))
        {
            Ok(job) => jobs.push(job),
            Err(err) => {
                log::error!(target: "lampo-vss", "dropping unreadable shadow job `{key}`: {err}");
                let _ = primary.remove(QUEUE_NAMESPACE, "", &key, false);
            }
        }
    }
    Ok(jobs)
}

/// Read how far the shadow has got.
///
/// Counts the backlog without reading the jobs: this is called to report lag,
/// and decoding a backlog's worth of mirrored values to produce a number would
/// make asking the question expensive. `dropped` is held by the caller, which
/// is the only place that knows about writes that never reached the queue.
pub fn read_lag(primary: &dyn LampoPersistenceBackend, dropped: u64) -> error::Result<ShadowLag> {
    Ok(ShadowLag {
        mirrored_seq: read_high_water(primary)?,
        pending: pending_keys(primary)?.len() as u64,
        dropped,
    })
}

/// The newest sequence number known to have reached the shadow.
pub fn read_high_water(primary: &dyn LampoPersistenceBackend) -> error::Result<u64> {
    read_counter(primary, HIGH_WATER_KEY)
}

/// Writes the shadow will never learn about, surviving restarts.
pub fn read_dropped(primary: &dyn LampoPersistenceBackend) -> error::Result<u64> {
    read_counter(primary, DROPPED_KEY)
}

/// Record that another write never made it into the queue.
pub fn write_dropped(primary: &dyn LampoPersistenceBackend, dropped: u64) -> error::Result<()> {
    primary.write(STATE_NAMESPACE, "", DROPPED_KEY, json::to_vec(&dropped)?)?;
    Ok(())
}

fn read_counter(primary: &dyn LampoPersistenceBackend, key: &str) -> error::Result<u64> {
    match primary.read(STATE_NAMESPACE, "", key) {
        Ok(buf) => Ok(json::from_slice::<u64>(&buf)?),
        Err(err) if err.kind() == lampo_common::ldk::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(err.into()),
    }
}

/// Record that everything up to `seq` has reached the shadow.
pub fn write_high_water(primary: &dyn LampoPersistenceBackend, seq: u64) -> error::Result<()> {
    primary.write(STATE_NAMESPACE, "", HIGH_WATER_KEY, json::to_vec(&seq)?)?;
    Ok(())
}

/// The sequence number to carry on from after a restart.
///
/// Reads the numbers out of the queue keys rather than the jobs themselves,
/// since the key is the sequence number.
pub fn next_seq(primary: &dyn LampoPersistenceBackend) -> error::Result<u64> {
    let queued = pending_keys(primary)?
        .last()
        .and_then(|key| key.parse::<u64>().ok())
        .unwrap_or_default();
    Ok(queued.max(read_high_water(primary)?) + 1)
}
