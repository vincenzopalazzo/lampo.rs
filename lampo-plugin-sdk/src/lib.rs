//! Lampo Plugin SDK
//!
//! Builder-pattern API for writing lampo plugins in Rust.
//!
//! # Example
//!
//! ```no_run
//! use lampo_plugin_sdk::Plugin;
//! use serde_json::Value;
//!
//! async fn handle_hello(_params: Value) -> Result<Value, String> {
//!     Ok(serde_json::json!({"message": "hello from plugin!"}))
//! }
//!
//! #[tokio::main]
//! async fn main() {
//!     Plugin::new()
//!         .rpc_method("hello", "Says hello", "[name]", handle_hello)
//!         .start()
//!         .await;
//! }
//! ```
mod io;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use lampo_plugin_common::manifest::{HookDecl, PluginOption, RpcMethodDecl};
use serde_json::Value;

/// Type alias for an async RPC handler.
pub type RpcHandler = Arc<
    dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>> + Send + Sync,
>;

/// Type alias for an async hook handler.
pub type HookHandler = Arc<
    dyn Fn(Value) -> Pin<Box<dyn Future<Output = HookResponse> + Send>> + Send + Sync,
>;

/// Type alias for an async notification handler.
pub type NotifyHandler = Arc<
    dyn Fn(Value) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
>;

/// Builder for constructing a lampo plugin.
pub struct Plugin {
    manifest: PluginManifest,
    rpc_handlers: HashMap<String, RpcHandler>,
    hook_handlers: HashMap<String, HookHandler>,
    notify_handlers: HashMap<String, NotifyHandler>,
}

impl Plugin {
    /// Create a new plugin builder.
    pub fn new() -> Self {
        Self {
            manifest: PluginManifest::default(),
            rpc_handlers: HashMap::new(),
            hook_handlers: HashMap::new(),
            notify_handlers: HashMap::new(),
        }
    }

    /// Register an RPC method with a handler.
    pub fn rpc_method<F, Fut>(mut self, name: &str, description: &str, usage: &str, handler: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.manifest.rpc_methods.push(RpcMethodDecl {
            name: name.to_string(),
            description: description.to_string(),
            usage: usage.to_string(),
        });
        let handler = Arc::new(move |params: Value| {
            Box::pin(handler(params)) as Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        });
        self.rpc_handlers.insert(name.to_string(), handler);
        self
    }

    /// Subscribe to a notification topic with a handler.
    pub fn subscribe<F, Fut>(mut self, topic: &str, handler: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.manifest.subscriptions.push(topic.to_string());
        let handler = Arc::new(move |params: Value| {
            Box::pin(handler(params)) as Pin<Box<dyn Future<Output = ()> + Send>>
        });
        self.notify_handlers.insert(topic.to_string(), handler);
        self
    }

    /// Register a hook handler.
    pub fn hook<F, Fut>(mut self, name: &str, handler: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HookResponse> + Send + 'static,
    {
        self.manifest.hooks.push(HookDecl {
            name: name.to_string(),
            before: vec![],
            after: vec![],
        });
        let handler = Arc::new(move |params: Value| {
            Box::pin(handler(params)) as Pin<Box<dyn Future<Output = HookResponse> + Send>>
        });
        self.hook_handlers.insert(name.to_string(), handler);
        self
    }

    /// Declare a plugin option.
    pub fn option(
        mut self,
        name: &str,
        opt_type: OptionType,
        default: Option<Value>,
        description: &str,
    ) -> Self {
        self.manifest.options.push(PluginOption {
            name: name.to_string(),
            opt_type,
            default,
            description: description.to_string(),
        });
        self
    }

    /// Mark the plugin as dynamic (can be started/stopped at runtime).
    pub fn dynamic(mut self, dynamic: bool) -> Self {
        self.manifest.dynamic = dynamic;
        self
    }

    /// Set the failure mode.
    pub fn failure_mode(mut self, mode: FailureMode) -> Self {
        self.manifest.failure_mode = mode;
        self
    }

    /// Start the plugin, reading from stdin and writing to stdout.
    ///
    /// This blocks until the daemon sends a shutdown signal.
    pub async fn start(self) {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        self.run(stdin, stdout).await;
    }

    /// Run the plugin with custom I/O (for testing).
    pub async fn run<R, W>(self, input: R, output: W)
    where
        R: tokio::io::AsyncRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        io::run_plugin(
            input,
            output,
            self.manifest,
            self.rpc_handlers,
            self.hook_handlers,
            self.notify_handlers,
        )
        .await;
    }
}

impl Default for Plugin {
    fn default() -> Self {
        Self::new()
    }
}

// Re-export commonly used types for plugin authors.
pub use lampo_plugin_common::manifest::{FailureMode, OptionType, PluginManifest};
pub use lampo_plugin_common::messages::{HookResponse, InitConfig};
