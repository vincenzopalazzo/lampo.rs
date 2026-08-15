//! Persistence backend interface.
//!
//! Lampo talks to its store through this trait rather than a concrete type, the
//! same way it talks to the chain through [`Backend`]. LDK's [`KVStoreSync`] is
//! a supertrait, so anything implementing this is usable everywhere LDK wants a
//! store, and lampo-specific concerns hang off the sub-trait.
//!
//! Backends must not acknowledge a write before it is durable: LDK broadcasts
//! the newest channel state it has persisted, so a write that is acked and then
//! lost can make the node force-close with a stale commitment.
//!
//! [`Backend`]: crate::backend::Backend
use crate::ldk::persister::fs_store::v1::FilesystemStore;
use crate::ldk::util::persist::KVStoreSync;

/// Persistence backend kind supported by lampo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceKind {
    Filesystem,
}

/// Persistence backend specification.
///
/// Implementors inherit [`KVStoreSync`], whose `read` must report a missing key
/// as [`io::ErrorKind::NotFound`]: callers such as the BOLT 12 payer proof store
/// tell "no record" from "store broken" by that error kind alone.
///
/// [`io::ErrorKind::NotFound`]: crate::ldk::io::ErrorKind::NotFound
pub trait LampoPersistenceBackend: KVStoreSync + Send + Sync {
    /// Return the kind of backend.
    fn kind(&self) -> PersistenceKind;
}

impl LampoPersistenceBackend for FilesystemStore {
    fn kind(&self) -> PersistenceKind {
        PersistenceKind::Filesystem
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
            Arc::new(FilesystemStore::new(self.0.clone()))
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
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
}
