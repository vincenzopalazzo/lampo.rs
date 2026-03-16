//! Plugin transport implementations.
//!
//! The `PluginTransport` trait abstracts how the daemon communicates
//! with a plugin. Implementations include:
//! - `StdioTransport`: local subprocess via stdin/stdout
//! - `GrpcTransport`: remote plugin via gRPC+mTLS (requires `grpc` feature)
pub mod stdio;
#[cfg(feature = "grpc")]
pub mod grpc;

use async_trait::async_trait;
use lampo_common::error;

/// Transport-agnostic plugin communication.
///
/// A plugin transport sends JSON-RPC requests/notifications
/// to a plugin and receives responses.
#[async_trait]
pub trait PluginTransport: Send + Sync {
    /// Send a JSON-RPC request and wait for the response.
    async fn request(&self, msg: serde_json::Value) -> error::Result<serde_json::Value>;

    /// Send a JSON-RPC notification (no response expected).
    async fn notify(&self, msg: serde_json::Value) -> error::Result<()>;

    /// Gracefully shut down the transport and the plugin process.
    async fn shutdown(&self) -> error::Result<()>;

    /// Check if the plugin is still alive.
    fn is_alive(&self) -> bool;
}
