//! LDK adapter for generic persistence
//!
//! This module provides an adapter that implements LDK's KVStore trait
//! using our generic Persister trait, allowing any persistence backend
//! to be used with LDK.

use std::sync::Arc;

use lampo_common::ldk::util::persist::KVStore;
use lampo_common::persist::Persister;

/// Adapter that implements LDK's KVStore using our generic Persister
pub struct LDKPersisterAdapter {
    persister: Arc<dyn Persister>,
}

impl LDKPersisterAdapter {
    /// Create a new LDK adapter
    pub fn new(persister: Arc<dyn Persister>) -> Self {
        Self { persister }
    }

    /// Build a key path in the format expected by LDK
    fn build_key(&self, primary_namespace: &str, secondary_namespace: &str, key: &str) -> String {
        if primary_namespace.is_empty() && secondary_namespace.is_empty() {
            key.to_string()
        } else if secondary_namespace.is_empty() {
            format!("{}/{}", primary_namespace, key)
        } else {
            format!("{}/{}/{}", primary_namespace, secondary_namespace, key)
        }
    }
}

impl KVStore for LDKPersisterAdapter {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, lampo_common::ldk::io::Error> {
        let full_key = self.build_key(primary_namespace, secondary_namespace, key);

        // We need to use a blocking version since LDK's KVStore trait is synchronous
        // In a real async runtime, we'd use a dedicated async runtime or thread pool
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { self.persister.read(&full_key).await })
        });

        match result {
            Ok(data) => Ok(data),
            Err(e) => {
                log::warn!("Failed to read key '{}': {}", full_key, e);
                Err(lampo_common::ldk::io::Error::new(
                    lampo_common::ldk::io::ErrorKind::NotFound,
                    e.to_string(),
                ))
            }
        }
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: &[u8],
    ) -> Result<(), lampo_common::ldk::io::Error> {
        let full_key = self.build_key(primary_namespace, secondary_namespace, key);

        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { self.persister.write(&full_key, buf).await })
        });

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                log::error!("Failed to write key '{}': {}", full_key, e);
                Err(lampo_common::ldk::io::Error::new(
                    lampo_common::ldk::io::ErrorKind::Other,
                    e.to_string(),
                ))
            }
        }
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        lazy: bool,
    ) -> Result<(), lampo_common::ldk::io::Error> {
        let full_key = self.build_key(primary_namespace, secondary_namespace, key);

        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { self.persister.remove(&full_key).await })
        });

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                if lazy {
                    log::warn!("Lazy remove failed for key '{}': {}", full_key, e);
                    Ok(()) // Don't fail for lazy removes
                } else {
                    log::error!("Failed to remove key '{}': {}", full_key, e);
                    Err(lampo_common::ldk::io::Error::new(
                        lampo_common::ldk::io::ErrorKind::Other,
                        e.to_string(),
                    ))
                }
            }
        }
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, lampo_common::ldk::io::Error> {
        let full_prefix = self.build_key(primary_namespace, secondary_namespace, "");

        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(async { self.persister.list(&full_prefix).await })
        });

        match result {
            Ok(keys) => {
                // Filter keys to only include those that match the prefix and namespace
                let filtered_keys: Vec<String> = keys
                    .into_iter()
                    .filter_map(|key| {
                        if key.starts_with(&full_prefix) {
                            // Remove the namespace prefix to get the relative key
                            Some(key[full_prefix.len()..].to_string())
                        } else {
                            None
                        }
                    })
                    .collect();
                Ok(filtered_keys)
            }
            Err(e) => {
                log::error!("Failed to list keys with prefix '{}': {}", full_prefix, e);
                Err(lampo_common::ldk::io::Error::new(
                    lampo_common::ldk::io::ErrorKind::Other,
                    e.to_string(),
                ))
            }
        }
    }
}
