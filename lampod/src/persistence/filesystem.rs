//! Filesystem-based persistence implementation for Lampo
//!
//! This implementation provides local filesystem storage using
//! LDK's FilesystemStore as the underlying storage mechanism.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use lampo_common::error;
use lampo_common::ldk::persister::fs_store::FilesystemStore;
use lampo_common::persist::{PersistenceKind, Persister};

/// Filesystem-based persister implementation
pub struct FilesystemPersister {
    /// Underlying LDK filesystem store
    store: Arc<FilesystemStore>,
    /// Base path for storage
    base_path: PathBuf,
}

impl FilesystemPersister {
    /// Create a new filesystem persister
    pub fn new<P: AsRef<Path>>(base_path: P) -> Self {
        let base_path = base_path.as_ref().to_path_buf();
        let store = Arc::new(FilesystemStore::new(base_path.clone()));

        Self { store, base_path }
    }

    /// Get the underlying LDK FilesystemStore for compatibility
    pub fn ldk_store(&self) -> Arc<FilesystemStore> {
        self.store.clone()
    }
}

#[async_trait]
impl Persister for FilesystemPersister {
    fn kind(&self) -> PersistenceKind {
        PersistenceKind::Filesystem
    }

    async fn write(&self, key: &str, data: &[u8]) -> error::Result<()> {
        use lampo_common::ldk::util::persist::KVStore;

        self.store
            .write("", "", key, data)
            .map_err(|e| error::anyhow!("Failed to write to filesystem: {:?}", e))?;
        Ok(())
    }

    async fn read(&self, key: &str) -> error::Result<Vec<u8>> {
        use lampo_common::ldk::util::persist::KVStore;

        self.store
            .read("", "", key)
            .map_err(|e| error::anyhow!("Failed to read from filesystem: {:?}", e))
    }

    async fn remove(&self, key: &str) -> error::Result<()> {
        use lampo_common::ldk::util::persist::KVStore;

        self.store
            .remove("", "", key, false)
            .map_err(|e| error::anyhow!("Failed to remove from filesystem: {:?}", e))?;
        Ok(())
    }

    async fn list(&self, prefix: &str) -> error::Result<Vec<String>> {
        use lampo_common::ldk::util::persist::KVStore;

        self.store
            .list("", "")
            .map_err(|e| error::anyhow!("Failed to list from filesystem: {:?}", e))
    }

    async fn exists(&self, key: &str) -> error::Result<bool> {
        use lampo_common::ldk::util::persist::KVStore;

        match self.store.read("", "", key) {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    async fn sync(&self) -> error::Result<()> {
        // Filesystem operations are typically synchronous, but we could
        // implement additional fsync() calls here if needed
        Ok(())
    }

    async fn initialize(&self) -> error::Result<()> {
        // Create base directory if it doesn't exist
        if !self.base_path.exists() {
            std::fs::create_dir_all(&self.base_path)
                .map_err(|e| error::anyhow!("Failed to create storage directory: {}", e))?;
        }
        Ok(())
    }

    async fn shutdown(&self) -> error::Result<()> {
        // No cleanup needed for filesystem storage
        Ok(())
    }
}
