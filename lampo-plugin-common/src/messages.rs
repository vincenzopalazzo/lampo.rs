//! Wire protocol message types for plugin communication.
//!
//! All messages are JSON-RPC 2.0. Requests have an `id` field,
//! notifications do not. Plugins read from stdin, write to stdout.
use serde::{Deserialize, Serialize};

use crate::manifest::PluginManifest;

/// Configuration sent to the plugin during `init`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitConfig {
    /// Path to lampo's data directory (e.g. "~/.lampo/testnet").
    pub lampo_dir: String,
    /// The network (e.g. "testnet", "bitcoin", "regtest", "signet").
    pub network: String,
    /// The node's public key (hex-encoded).
    #[serde(default)]
    pub node_id: String,
    /// Resolved option values from the manifest.
    #[serde(default)]
    pub options: serde_json::Map<String, serde_json::Value>,
}

/// The `init` response from a plugin.
///
/// If the result contains a `disable` key, the plugin wants to
/// disable itself. Otherwise it initialized successfully.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitResponse {
    /// If set, the plugin wants to disable itself with this reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable: Option<String>,
}

impl InitResponse {
    /// Check if the plugin wants to disable itself.
    pub fn is_disabled(&self) -> bool {
        self.disable.is_some()
    }
}

/// Hook response from a plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum HookResponse {
    /// Pass to next plugin or default behavior.
    Continue {
        #[serde(skip_serializing_if = "Option::is_none")]
        payload: Option<serde_json::Value>,
    },
    /// Short-circuit the hook chain with this result.
    Complete { payload: serde_json::Value },
    /// Reject/abort the operation.
    Reject {
        #[serde(default)]
        message: String,
    },
}

impl Default for HookResponse {
    fn default() -> Self {
        Self::Continue { payload: None }
    }
}

/// A plugin notification (daemon → plugin, no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginNotification {
    /// The notification topic (e.g. "channel_ready", "payment").
    pub method: String,
    /// The notification payload.
    pub params: serde_json::Value,
    /// JSON-RPC version — always "2.0".
    pub jsonrpc: String,
}

impl PluginNotification {
    pub fn new(topic: &str, params: serde_json::Value) -> Self {
        Self {
            method: topic.to_owned(),
            params,
            jsonrpc: "2.0".to_owned(),
        }
    }
}

/// A generic JSON-RPC 2.0 request (daemon → plugin).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRequest {
    pub method: String,
    pub params: serde_json::Value,
    pub id: serde_json::Value,
    pub jsonrpc: String,
}

impl PluginRequest {
    pub fn new(method: &str, params: serde_json::Value, id: u64) -> Self {
        Self {
            method: method.to_owned(),
            params,
            id: serde_json::Value::Number(id.into()),
            jsonrpc: "2.0".to_owned(),
        }
    }
}

/// A generic JSON-RPC 2.0 response (plugin → daemon).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<PluginRpcError>,
    pub id: serde_json::Value,
    pub jsonrpc: String,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Wraps a `PluginManifest` into a manifest response.
/// This is the result field of the `getmanifest` response.
pub type ManifestResponse = PluginManifest;
