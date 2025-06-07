//! VSS (Versioned Storage Service) persistence implementation for Lampo
//!
//! This implementation provides cloud-based storage using the VSS protocol
//! for recovery and multi-device access capabilities.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use lampo_common::error;
use lampo_common::persist::{PersistenceKind, Persister};
use tokio::sync::RwLock;

/// VSS client configuration
#[derive(Debug, Clone)]
pub struct VSSConfig {
    /// VSS server endpoint URL
    pub endpoint: String,
    /// Store ID for this node
    pub store_id: String,
    /// Authentication token (optional)
    pub auth_token: Option<String>,
    /// Custom headers for requests
    pub headers: HashMap<String, String>,
    /// Client-side encryption key (derived from wallet seed)
    pub encryption_key: Option<[u8; 32]>,
}

/// VSS-based persister implementation
pub struct VSSPersister {
    /// VSS client configuration
    config: VSSConfig,
    /// In-memory cache for frequently accessed data
    cache: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    /// HTTP client for VSS requests
    client: reqwest::Client,
}

impl VSSPersister {
    /// Create a new VSS persister
    pub fn new(config: VSSConfig) -> Self {
        let client = reqwest::Client::new();
        let cache = Arc::new(RwLock::new(HashMap::new()));

        Self {
            config,
            cache,
            client,
        }
    }

    /// Encrypt data if encryption key is provided
    fn encrypt_data(&self, data: &[u8]) -> error::Result<Vec<u8>> {
        if let Some(key) = &self.config.encryption_key {
            // TODO: Implement client-side encryption using AES-GCM or ChaCha20Poly1305
            // For now, return data as-is
            log::warn!("Client-side encryption not yet implemented for VSS");
            Ok(data.to_vec())
        } else {
            Ok(data.to_vec())
        }
    }

    /// Decrypt data if encryption key is provided
    fn decrypt_data(&self, data: &[u8]) -> error::Result<Vec<u8>> {
        if let Some(_key) = &self.config.encryption_key {
            // TODO: Implement client-side decryption
            // For now, return data as-is
            log::warn!("Client-side decryption not yet implemented for VSS");
            Ok(data.to_vec())
        } else {
            Ok(data.to_vec())
        }
    }

    /// Build VSS API URL for a given key
    fn build_url(&self, key: &str) -> String {
        format!(
            "{}/v1/{}/{}",
            self.config.endpoint, self.config.store_id, key
        )
    }

    /// Build request headers
    fn build_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();

        // Add custom headers
        for (name, value) in &self.config.headers {
            if let (Ok(name), Ok(value)) = (
                reqwest::header::HeaderName::from_bytes(name.as_bytes()),
                reqwest::header::HeaderValue::from_str(value),
            ) {
                headers.insert(name, value);
            }
        }

        // Add auth token if provided
        if let Some(token) = &self.config.auth_token {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token))
            {
                headers.insert(reqwest::header::AUTHORIZATION, value);
            }
        }

        headers
    }
}

#[async_trait]
impl Persister for VSSPersister {
    fn kind(&self) -> PersistenceKind {
        PersistenceKind::VSS
    }

    async fn write(&self, key: &str, data: &[u8]) -> error::Result<()> {
        let encrypted_data = self.encrypt_data(data)?;
        let url = self.build_url(key);
        let headers = self.build_headers();

        let response = self
            .client
            .put(&url)
            .headers(headers)
            .body(encrypted_data.clone())
            .send()
            .await
            .map_err(|e| error::anyhow!("VSS write request failed: {}", e))?;

        if response.status().is_success() {
            // Update cache
            self.cache
                .write()
                .await
                .insert(key.to_string(), encrypted_data);
            Ok(())
        } else {
            error::bail!("VSS write failed with status: {}", response.status())
        }
    }

    async fn read(&self, key: &str) -> error::Result<Vec<u8>> {
        // Check cache first
        {
            let cache = self.cache.read().await;
            if let Some(data) = cache.get(key) {
                return self.decrypt_data(data);
            }
        }

        let url = self.build_url(key);
        let headers = self.build_headers();

        let response = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| error::anyhow!("VSS read request failed: {}", e))?;

        if response.status().is_success() {
            let encrypted_data = response
                .bytes()
                .await
                .map_err(|e| error::anyhow!("Failed to read VSS response: {}", e))?
                .to_vec();

            // Update cache
            self.cache
                .write()
                .await
                .insert(key.to_string(), encrypted_data.clone());

            self.decrypt_data(&encrypted_data)
        } else if response.status() == reqwest::StatusCode::NOT_FOUND {
            error::bail!("Key not found: {}", key)
        } else {
            error::bail!("VSS read failed with status: {}", response.status())
        }
    }

    async fn remove(&self, key: &str) -> error::Result<()> {
        let url = self.build_url(key);
        let headers = self.build_headers();

        let response = self
            .client
            .delete(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| error::anyhow!("VSS delete request failed: {}", e))?;

        if response.status().is_success() {
            // Remove from cache
            self.cache.write().await.remove(key);
            Ok(())
        } else {
            error::bail!("VSS delete failed with status: {}", response.status())
        }
    }

    async fn list(&self, prefix: &str) -> error::Result<Vec<String>> {
        let url = format!("{}/v1/{}", self.config.endpoint, self.config.store_id);
        let headers = self.build_headers();

        let response = self
            .client
            .get(&url)
            .headers(headers)
            .query(&[("prefix", prefix)])
            .send()
            .await
            .map_err(|e| error::anyhow!("VSS list request failed: {}", e))?;

        if response.status().is_success() {
            let keys: Vec<String> = response
                .json()
                .await
                .map_err(|e| error::anyhow!("Failed to parse VSS list response: {}", e))?;
            Ok(keys)
        } else {
            error::bail!("VSS list failed with status: {}", response.status())
        }
    }

    async fn exists(&self, key: &str) -> error::Result<bool> {
        // Check cache first
        {
            let cache = self.cache.read().await;
            if cache.contains_key(key) {
                return Ok(true);
            }
        }

        let url = self.build_url(key);
        let headers = self.build_headers();

        let response = self
            .client
            .head(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| error::anyhow!("VSS exists request failed: {}", e))?;

        Ok(response.status().is_success())
    }

    async fn sync(&self) -> error::Result<()> {
        // VSS operations are synchronous by nature as they go over HTTP
        // We could implement batching or delayed writes here if needed
        Ok(())
    }

    async fn initialize(&self) -> error::Result<()> {
        // Test connection to VSS server
        let url = format!("{}/v1/health", self.config.endpoint);
        let headers = self.build_headers();

        let response = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| error::anyhow!("VSS health check failed: {}", e))?;

        if response.status().is_success() {
            log::info!("VSS persister initialized successfully");
            Ok(())
        } else {
            error::bail!("VSS health check failed with status: {}", response.status())
        }
    }

    async fn shutdown(&self) -> error::Result<()> {
        // Clear cache
        self.cache.write().await.clear();
        log::info!("VSS persister shutdown completed");
        Ok(())
    }
}
