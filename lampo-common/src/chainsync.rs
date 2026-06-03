//! Backend-agnostic chain-sync coordination.
//!
//! `ChainSyncCoordinator` is a small, dependency-free state holder that lets
//! the chain backend, the on-chain wallet, and the JSON-RPC layer agree on
//! where the node is in its initial sync — without any of them depending on
//! each other. It carries **no** LDK or BDK types on purpose: the concrete
//! `Backend` drives the transitions, the wallet reports progress, and
//! `getinfo` reads the state. See `docs/designs/unified-chain-sync.md`.
//!
//! In this first PR the coordinator is wired in but not yet driven; later PRs
//! call `mark_listeners_synced` / `mark_running` from the sync path.
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::watch;

/// Lifecycle of the node's initial chain sync.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SyncState {
    /// Initial listener sync has not completed yet.
    #[default]
    PendingInitialSync,
    /// LDK chain listeners are caught up to tip; wallet may still be scanning.
    ListenersSynced,
    /// Initial sync fully complete; the node is following the chain tip.
    Running,
}

/// Sentinel for "no wallet scan height reported yet" stored in the atomic.
const NO_HEIGHT: u64 = u64::MAX;

/// Backend-agnostic coordinator for the node's chain-sync state.
///
/// Cheap to clone behind an `Arc`; all methods take `&self`.
pub struct ChainSyncCoordinator {
    /// Current lifecycle state. A `watch` channel so callers can `await`
    /// transitions (`wait_initial_sync_complete`) as well as read the latest
    /// value.
    state: watch::Sender<SyncState>,
    /// Latest wallet scan height, or `NO_HEIGHT` when none has been reported.
    wallet_scan_height: AtomicU64,
}

impl ChainSyncCoordinator {
    pub fn new() -> Self {
        let (state, _) = watch::channel(SyncState::PendingInitialSync);
        Self {
            state,
            wallet_scan_height: AtomicU64::new(NO_HEIGHT),
        }
    }

    /// Current lifecycle state.
    pub fn state(&self) -> SyncState {
        *self.state.borrow()
    }

    /// Advance to `ListenersSynced` once the LDK listeners reach tip.
    ///
    /// Only moves forward from `PendingInitialSync`; never regresses a node
    /// that is already `Running`.
    pub fn mark_listeners_synced(&self) {
        self.state.send_if_modified(|state| {
            if matches!(state, SyncState::PendingInitialSync) {
                *state = SyncState::ListenersSynced;
                true
            } else {
                false
            }
        });
    }

    /// Advance to `Running`: the initial sync is fully complete.
    ///
    /// Only advances from `ListenersSynced` (a no-op from `Running`, and
    /// defensively a no-op from `PendingInitialSync` so the state machine is
    /// self-consistent regardless of caller).
    pub fn mark_running(&self) {
        self.state.send_if_modified(|state| {
            if matches!(state, SyncState::ListenersSynced) {
                *state = SyncState::Running;
                true
            } else {
                false
            }
        });
    }

    /// Latest reported wallet scan height, if any.
    pub fn wallet_scan_height(&self) -> Option<u32> {
        match self.wallet_scan_height.load(Ordering::Relaxed) {
            NO_HEIGHT => None,
            height => Some(height as u32),
        }
    }

    /// Report the wallet's current scan height (live progress during catch-up).
    pub fn set_wallet_scan_height(&self, height: u32) {
        self.wallet_scan_height
            .store(u64::from(height), Ordering::Relaxed);
    }

    /// Resolve once the full initial sync has completed (state is `Running`).
    /// Used by the `sync_wallets` RPC to block until the node is caught up.
    pub async fn wait_initial_sync_complete(&self) {
        let mut rx = self.state.subscribe();
        loop {
            if matches!(*rx.borrow_and_update(), SyncState::Running) {
                return;
            }
            // The coordinator owns `self.state`, so the sender never drops while
            // we hold `&self`; `changed()` therefore cannot error here, but if it
            // ever did we simply stop waiting rather than spin.
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Whether the LDK chain listeners have completed their initial sync.
    pub fn chain_listeners_synced(&self) -> bool {
        !matches!(self.state(), SyncState::PendingInitialSync)
    }

    /// Whether the full initial sync (listeners + wallet) has completed.
    pub fn initial_sync_complete(&self) -> bool {
        matches!(self.state(), SyncState::Running)
    }

    /// Whether an initial sync is still in progress (not yet `Running`).
    pub fn sync_in_progress(&self) -> bool {
        !matches!(self.state(), SyncState::Running)
    }
}

impl Default for ChainSyncCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state_is_pending() {
        let coord = ChainSyncCoordinator::new();
        assert_eq!(coord.state(), SyncState::PendingInitialSync);
        assert!(!coord.chain_listeners_synced());
        assert!(!coord.initial_sync_complete());
        assert!(coord.sync_in_progress());
        assert_eq!(coord.wallet_scan_height(), None);
    }

    #[test]
    fn mark_listeners_synced_advances_once() {
        let coord = ChainSyncCoordinator::new();
        coord.mark_listeners_synced();
        assert_eq!(coord.state(), SyncState::ListenersSynced);
        assert!(coord.chain_listeners_synced());
        assert!(!coord.initial_sync_complete());
        assert!(coord.sync_in_progress());
    }

    #[test]
    fn mark_listeners_synced_does_not_regress_running() {
        let coord = ChainSyncCoordinator::new();
        coord.mark_listeners_synced();
        coord.mark_running();
        // A late listener-sync signal must not pull a Running node backwards.
        coord.mark_listeners_synced();
        assert_eq!(coord.state(), SyncState::Running);
        assert!(coord.initial_sync_complete());
        assert!(!coord.sync_in_progress());
    }

    #[test]
    fn mark_running_requires_listeners_synced_first() {
        let coord = ChainSyncCoordinator::new();
        // Defensive: never jump straight from Pending to Running.
        coord.mark_running();
        assert_eq!(coord.state(), SyncState::PendingInitialSync);
        coord.mark_listeners_synced();
        coord.mark_running();
        assert_eq!(coord.state(), SyncState::Running);
    }

    #[test]
    fn wallet_scan_height_roundtrip() {
        let coord = ChainSyncCoordinator::new();
        assert_eq!(coord.wallet_scan_height(), None);
        coord.set_wallet_scan_height(307_000);
        assert_eq!(coord.wallet_scan_height(), Some(307_000));
    }
}
