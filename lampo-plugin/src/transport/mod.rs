//! Plugin transport implementations.
//!
//! A local plugin is a gRPC server. The daemon spawns it with
//! `--lampo-listen` and dials that loopback address. Stdio is not a
//! plugin transport.
#[cfg(feature = "grpc")]
pub mod grpc;
#[cfg(feature = "grpc")]
pub mod local;
#[cfg(feature = "grpc")]
pub mod uds;

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

    /// True when `method` is already being handled by this plugin.
    ///
    /// `foo` calling `foo` waits on itself. Skip that method so the
    /// built-in handler can answer, or the call fails closed. A different
    /// method (`yoooo`) must still be forwarded: gRPC runs it on another
    /// task. Default is false.
    fn method_in_flight(&self, method: &str) -> bool {
        let _ = method;
        false
    }
}
