//! Persistence abstraction for Lampo
//!
//! This module provides a generic persistence interface that allows
//! different storage backends to be used for LDK channel monitors,
//! channel manager state, and other persistent data.

use async_trait::async_trait;

use crate::error;

/// Persistence kind supported by lampo
#[derive(Debug, Clone)]
pub enum PersistenceKind {
    /// Local filesystem persistence
    Filesystem,
    /// VSS (Versioned Storage Service) persistence
    VSS,
    /// SQL database persistence
    Database,
}

/// Generic persistence trait for storing LDK and application data
#[async_trait]
pub trait Persister: Send + Sync {
    /// Return the kind of persister
    fn kind(&self) -> PersistenceKind;

    /// Write data to storage with the given key
    async fn write(&self, key: &str, data: &[u8]) -> error::Result<()>;

    /// Read data from storage with the given key
    async fn read(&self, key: &str) -> error::Result<Vec<u8>>;

    /// Remove data from storage with the given key
    async fn remove(&self, key: &str) -> error::Result<()>;

    /// List all keys with the given prefix
    async fn list(&self, prefix: &str) -> error::Result<Vec<String>>;

    /// Check if a key exists in storage
    async fn exists(&self, key: &str) -> error::Result<bool>;

    /// Sync/flush any pending writes to ensure durability
    async fn sync(&self) -> error::Result<()>;

    /// Initialize the persister (create directories, connect to remote service, etc.)
    async fn initialize(&self) -> error::Result<()>;

    /// Shutdown the persister gracefully
    async fn shutdown(&self) -> error::Result<()>;
}
