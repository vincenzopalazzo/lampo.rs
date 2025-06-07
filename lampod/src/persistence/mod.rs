//! Persistence module implementation for lampo
//!
//! This module provides multiple persistence backends for storing
//! LDK channel monitors, channel manager state, and other data.
//!
//! Supported backends:
//! - Filesystem: Local file storage using LDK's FilesystemStore
//! - VSS: Cloud storage using Versioned Storage Service
//! - Database: SQL database storage (future extension)

use std::sync::Arc;

use lampo_common::persist::Persister;

pub mod filesystem;
pub mod ldk_adapter;
pub mod vss;

pub use filesystem::FilesystemPersister;
pub use ldk_adapter::LDKPersisterAdapter;
pub use vss::{VSSConfig, VSSPersister};

// Legacy type alias for backwards compatibility
use lampo_common::ldk::persister::fs_store::FilesystemStore;
pub type LampoPersistence = FilesystemStore;

/// Persistence factory for creating different types of persisters
pub struct PersistenceFactory;

impl PersistenceFactory {
    /// Create a filesystem persister
    pub fn filesystem<P: AsRef<std::path::Path>>(path: P) -> Arc<dyn Persister> {
        Arc::new(FilesystemPersister::new(path))
    }

    /// Create a VSS persister
    pub fn vss(config: VSSConfig) -> Arc<dyn Persister> {
        Arc::new(VSSPersister::new(config))
    }

    /// Create an LDK adapter for any persister
    pub fn ldk_adapter(persister: Arc<dyn Persister>) -> Arc<LDKPersisterAdapter> {
        Arc::new(LDKPersisterAdapter::new(persister))
    }
}
