//! Persistence wiring for lampod.
//!
//! The daemon holds its store as [`LampoPersistenceBackend`], so the choice of
//! backend lives here and nowhere else. Today the only backend is LDK's
//! filesystem store; database backends plug in by extending [`persistence_for`].
//!
//! N.B: the persistence has not been hardened for production use yet. Run a
//! node with funds you can afford to lose.
use std::sync::Arc;

use lampo_common::ldk::persister::fs_store::v1::FilesystemStore;
use lampo_common::persist::LampoPersistenceBackend;

/// Build the persistence backend rooted at `root_path`.
pub fn persistence_for(root_path: &str) -> Arc<dyn LampoPersistenceBackend> {
    Arc::new(FilesystemStore::new(root_path.into()))
}
