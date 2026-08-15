//! Durable swap store: one JSON file per swap, written atomically
//! (tmp + rename) before any transition is acted on. Losing a swap
//! record across a restart means losing the ability to resolve it, so
//! persistence comes first, action second — same rule lampo's hold
//! registry follows.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use lampo_common::error;

use crate::swap::Swap;

pub struct SwapStore {
    dir: PathBuf,
    swaps: Mutex<HashMap<String, Swap>>,
}

impl SwapStore {
    pub fn new(dir: PathBuf) -> error::Result<Self> {
        std::fs::create_dir_all(&dir)
            .map_err(|err| error::anyhow!("cannot create the swap dir: {err}"))?;
        let mut swaps = HashMap::new();
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let content = std::fs::read_to_string(&path)?;
            let swap: Swap = serde_json::from_str(&content)
                .map_err(|err| error::anyhow!("corrupted swap record `{path:?}`: {err}"))?;
            swaps.insert(swap.id(), swap);
        }
        Ok(Self {
            dir,
            swaps: Mutex::new(swaps),
        })
    }

    /// Persist the swap, then make it visible in memory. The write is
    /// atomic: a crash mid-write leaves the previous record, never a
    /// truncated one.
    pub fn persist(&self, swap: &Swap) -> error::Result<()> {
        let path = self.dir.join(format!("{}.json", swap.id()));
        let tmp = self.dir.join(format!("{}.json.tmp", swap.id()));
        let content = serde_json::to_string_pretty(swap)?;
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &path)?;
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        self.swaps.lock().unwrap().insert(swap.id(), swap.clone());
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<Swap> {
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        self.swaps.lock().unwrap().get(id).cloned()
    }

    pub fn pending(&self) -> Vec<Swap> {
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        self.swaps
            .lock()
            .unwrap()
            .values()
            .filter(|swap| !swap.is_terminal())
            .cloned()
            .collect()
    }

    pub fn list(&self) -> Vec<Swap> {
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        self.swaps.lock().unwrap().values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swap::{now, Direction, State};

    /// A directory nobody else in this process will pick. Timestamps
    /// are not enough: tests run in parallel and two of them can read
    /// the same nanosecond, then share a store and wipe each other.
    fn tmp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "swapd-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn a_swap(hash: &str) -> Swap {
        Swap {
            payment_hash: Some(hash.to_owned()),
            offer_id: None,
            direction: Direction::SparkToLn,
            state: State::Quoted,
            amount_msat: 42_000,
            spark_amount_sat: 42,
            lampo_payment_id: Some("11".repeat(32)),
            spark_transfer_id: None,
            preimage: None,
            counterparty_spark_address: None,
            spark_locked_at: None,
            offer: "lno1qq".to_owned(),
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn swaps_survive_a_reload() {
        let dir = tmp_dir();
        let store = SwapStore::new(dir.clone()).unwrap();
        let mut swap = a_swap(&"ab".repeat(32));
        store.persist(&swap).unwrap();
        swap.transition(State::LnPaying).unwrap();
        store.persist(&swap).unwrap();

        let reloaded = SwapStore::new(dir.clone()).unwrap();
        let got = reloaded.get(&swap.id()).unwrap();
        assert_eq!(got.state, State::LnPaying);
        assert_eq!(reloaded.pending().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

}
