//! Plugin lifecycle management and RPC dispatch.
//!
//! `PluginManager` implements `ExternalHandler` and slots into
//! the existing lampo handler chain. It manages plugin lifecycle
//! (start, stop) and routes RPC calls to the correct plugin.
//!
//! It also provides:
//! - **Hook chain execution**: synchronous interception points
//! - **Notification dispatch**: async event forwarding to plugins
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use lampo_common::error;
use lampo_common::handler::ExternalHandler;
use lampo_common::json;
use lampo_common::jsonrpc::Request;
use lampo_plugin_common::hooks::HookPoint;
use lampo_plugin_common::manifest::FailureMode;
use lampo_plugin_common::messages::{HookResponse, InitConfig, InitResponse};
use lampo_plugin_common::topics;
use lampo_plugin_common::PluginManifest;
use tokio::sync::RwLock;

use crate::transport::stdio::StdioTransport;
use crate::transport::PluginTransport;
#[cfg(feature = "grpc")]
use crate::transport::grpc::{GrpcConfig, GrpcTransport};

/// State of a running plugin.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginState {
    Starting,
    Running,
    Stopping,
    Stopped,
}

/// A running plugin instance.
pub struct PluginInstance {
    /// Plugin name (derived from the binary name or manifest).
    pub name: String,
    /// Path to the plugin binary.
    pub path: String,
    /// The plugin's declared capabilities.
    pub manifest: PluginManifest,
    /// Transport for communicating with the plugin.
    pub transport: Box<dyn PluginTransport>,
    /// Current state.
    pub state: PluginState,
    /// RPC methods registered by this plugin.
    pub registered_methods: HashSet<String>,
}

/// Manages plugin lifecycles and dispatches RPC calls.
///
/// Uses name-based indexing (not positional) to avoid index invalidation
/// when plugins are added or removed.
pub struct PluginManager {
    /// Running plugins keyed by name.
    plugins: RwLock<HashMap<String, Arc<RwLock<PluginInstance>>>>,
    /// RPC method name → plugin name that owns it.
    method_index: RwLock<HashMap<String, String>>,
    /// Hook name → ordered list of plugin names.
    hook_index: RwLock<HashMap<String, Vec<String>>>,
    /// Notification topic → list of plugin names.
    subscription_index: RwLock<HashMap<String, Vec<String>>>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            plugins: RwLock::new(HashMap::new()),
            method_index: RwLock::new(HashMap::new()),
            hook_index: RwLock::new(HashMap::new()),
            subscription_index: RwLock::new(HashMap::new()),
        }
    }

    /// Start a local plugin from a binary path.
    ///
    /// Performs the two-phase handshake:
    /// 1. `getmanifest` — ask plugin what it can do
    /// 2. `init` — send configuration to the plugin
    pub async fn start_plugin(
        &self,
        path: &str,
        init_config: &InitConfig,
    ) -> error::Result<String> {
        log::info!(target: "plugin", "starting plugin: {}", path);

        let transport = StdioTransport::new(path).await?;

        // Phase 1: getmanifest
        let manifest_req = serde_json::json!({
            "method": "getmanifest",
            "params": {},
            "jsonrpc": "2.0"
        });
        let manifest_resp = transport.request(manifest_req).await?;

        // Check for JSON-RPC error in response
        if let Some(error) = manifest_resp.get("error") {
            let msg = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            error::bail!("plugin `{}`: getmanifest failed: {}", path, msg);
        }

        let manifest_result = manifest_resp
            .get("result")
            .ok_or_else(|| error::anyhow!("plugin `{}`: getmanifest returned no result", path))?;
        let manifest: PluginManifest = serde_json::from_value(manifest_result.clone())
            .map_err(|e| error::anyhow!("plugin `{}`: invalid manifest: {}", path, e))?;

        log::info!(
            target: "plugin",
            "plugin `{}`: manifest received, {} rpc methods, {} hooks, {} subscriptions",
            path,
            manifest.rpc_methods.len(),
            manifest.hooks.len(),
            manifest.subscriptions.len()
        );

        // Derive plugin name from the binary path
        let plugin_name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path)
            .to_string();

        // Check for method name collisions
        {
            let method_index = self.method_index.read().await;
            for method in &manifest.rpc_methods {
                if method_index.contains_key(&method.name) {
                    error::bail!(
                        "plugin `{}`: method `{}` already registered by another plugin",
                        plugin_name,
                        method.name
                    );
                }
            }
        }

        // Phase 2: init
        let init_req = serde_json::json!({
            "method": "init",
            "params": serde_json::to_value(init_config)?,
            "jsonrpc": "2.0"
        });
        let init_resp = transport.request(init_req).await?;

        // Check for JSON-RPC error in init response
        if let Some(error) = init_resp.get("error") {
            let msg = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            error::bail!("plugin `{}`: init failed: {}", plugin_name, msg);
        }

        // Check if the plugin wants to disable itself
        if let Some(result) = init_resp.get("result") {
            if let Ok(init_result) = serde_json::from_value::<InitResponse>(result.clone()) {
                if let Some(disable) = init_result.disable {
                    log::warn!(
                        target: "plugin",
                        "plugin `{}` disabled itself: {}",
                        plugin_name, disable
                    );
                    transport.shutdown().await?;
                    error::bail!("plugin `{}` disabled itself: {}", plugin_name, disable);
                }
            }
        }

        // Register the plugin
        let registered_methods: HashSet<String> =
            manifest.rpc_methods.iter().map(|m| m.name.clone()).collect();
        let hook_names: Vec<String> = manifest.hooks.iter().map(|h| h.name.clone()).collect();
        let subscriptions: Vec<String> = manifest.subscriptions.clone();

        let instance = PluginInstance {
            name: plugin_name.clone(),
            path: path.to_string(),
            manifest,
            transport: Box::new(transport),
            state: PluginState::Running,
            registered_methods: registered_methods.clone(),
        };

        let instance = Arc::new(RwLock::new(instance));

        // Add to the plugins map and update all indices
        {
            let mut plugins = self.plugins.write().await;
            plugins.insert(plugin_name.clone(), instance);

            let mut method_index = self.method_index.write().await;
            for method_name in &registered_methods {
                log::info!(
                    target: "plugin",
                    "plugin `{}`: registered method `{}`",
                    plugin_name, method_name
                );
                method_index.insert(method_name.clone(), plugin_name.clone());
            }

            // Register hooks
            let mut hook_index = self.hook_index.write().await;
            for hook_name in hook_names {
                log::info!(
                    target: "plugin",
                    "plugin `{}`: registered hook `{}`",
                    plugin_name, hook_name
                );
                hook_index.entry(hook_name).or_default().push(plugin_name.clone());
            }

            // Register subscriptions
            let mut sub_index = self.subscription_index.write().await;
            for topic in subscriptions {
                log::info!(
                    target: "plugin",
                    "plugin `{}`: subscribed to `{}`",
                    plugin_name, topic
                );
                sub_index.entry(topic).or_default().push(plugin_name.clone());
            }
        }

        log::info!(target: "plugin", "plugin `{}` started successfully", plugin_name);
        Ok(plugin_name)
    }

    /// Start a remote plugin via gRPC.
    ///
    /// The daemon connects as a gRPC client to the remote plugin server,
    /// then performs the same two-phase handshake as local plugins.
    #[cfg(feature = "grpc")]
    pub async fn start_remote_plugin(
        &self,
        config: GrpcConfig,
        init_config: &InitConfig,
    ) -> error::Result<String> {
        let endpoint = config.endpoint.clone();
        log::info!(target: "plugin", "connecting to remote plugin: {}", endpoint);

        let transport = GrpcTransport::connect(config).await?;

        // Same two-phase handshake as local plugins
        let manifest_req = serde_json::json!({
            "method": "getmanifest",
            "params": {},
            "jsonrpc": "2.0"
        });
        let manifest_resp = transport.request(manifest_req).await?;

        // Check for JSON-RPC error
        if let Some(error) = manifest_resp.get("error") {
            let msg = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            error::bail!("remote plugin `{}`: getmanifest failed: {}", endpoint, msg);
        }

        let manifest_result = manifest_resp
            .get("result")
            .ok_or_else(|| error::anyhow!("remote plugin `{}`: getmanifest returned no result", endpoint))?;
        let manifest: PluginManifest = serde_json::from_value(manifest_result.clone())
            .map_err(|e| error::anyhow!("remote plugin `{}`: invalid manifest: {}", endpoint, e))?;

        log::info!(
            target: "plugin",
            "remote plugin `{}`: manifest received, {} rpc methods, {} hooks, {} subscriptions",
            endpoint,
            manifest.rpc_methods.len(),
            manifest.hooks.len(),
            manifest.subscriptions.len()
        );

        // Derive name from endpoint (host:port)
        let plugin_name = endpoint
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .replace([':', '/'], "_");

        // Check for method collisions
        {
            let method_index = self.method_index.read().await;
            for method in &manifest.rpc_methods {
                if method_index.contains_key(&method.name) {
                    error::bail!(
                        "remote plugin `{}`: method `{}` already registered by another plugin",
                        plugin_name,
                        method.name
                    );
                }
            }
        }

        // Phase 2: init
        let init_req = serde_json::json!({
            "method": "init",
            "params": serde_json::to_value(init_config)?,
            "jsonrpc": "2.0"
        });
        let init_resp = transport.request(init_req).await?;

        // Check for JSON-RPC error
        if let Some(error) = init_resp.get("error") {
            let msg = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            error::bail!("remote plugin `{}`: init failed: {}", plugin_name, msg);
        }

        if let Some(result) = init_resp.get("result") {
            if let Ok(init_result) = serde_json::from_value::<InitResponse>(result.clone()) {
                if let Some(disable) = init_result.disable {
                    log::warn!(
                        target: "plugin",
                        "remote plugin `{}` disabled itself: {}",
                        plugin_name, disable
                    );
                    transport.shutdown().await?;
                    error::bail!("remote plugin `{}` disabled itself: {}", plugin_name, disable);
                }
            }
        }

        // Register
        let registered_methods: HashSet<String> =
            manifest.rpc_methods.iter().map(|m| m.name.clone()).collect();
        let hook_names: Vec<String> = manifest.hooks.iter().map(|h| h.name.clone()).collect();
        let subscriptions: Vec<String> = manifest.subscriptions.clone();

        let instance = PluginInstance {
            name: plugin_name.clone(),
            path: endpoint.clone(),
            manifest,
            transport: Box::new(transport),
            state: PluginState::Running,
            registered_methods: registered_methods.clone(),
        };

        let instance = Arc::new(RwLock::new(instance));

        {
            let mut plugins = self.plugins.write().await;
            plugins.insert(plugin_name.clone(), instance);

            let mut method_index = self.method_index.write().await;
            for method_name in &registered_methods {
                log::info!(
                    target: "plugin",
                    "remote plugin `{}`: registered method `{}`",
                    plugin_name, method_name
                );
                method_index.insert(method_name.clone(), plugin_name.clone());
            }

            let mut hook_index = self.hook_index.write().await;
            for hook_name in hook_names {
                hook_index.entry(hook_name).or_default().push(plugin_name.clone());
            }

            let mut sub_index = self.subscription_index.write().await;
            for topic in subscriptions {
                sub_index.entry(topic).or_default().push(plugin_name.clone());
            }
        }

        log::info!(target: "plugin", "remote plugin `{}` started successfully", plugin_name);
        Ok(plugin_name)
    }

    /// Stop a plugin by name.
    pub async fn stop_plugin(&self, name: &str) -> error::Result<()> {
        // Remove from plugins map
        let plugin = {
            let mut plugins = self.plugins.write().await;
            plugins
                .remove(name)
                .ok_or_else(|| error::anyhow!("plugin `{}` not found", name))?
        };

        // Remove from all indices
        {
            let mut method_index = self.method_index.write().await;
            method_index.retain(|_, plugin_name| plugin_name != name);
        }
        {
            let mut hook_index = self.hook_index.write().await;
            for entries in hook_index.values_mut() {
                entries.retain(|plugin_name| plugin_name != name);
            }
            // Remove empty entries
            hook_index.retain(|_, entries| !entries.is_empty());
        }
        {
            let mut sub_index = self.subscription_index.write().await;
            for entries in sub_index.values_mut() {
                entries.retain(|plugin_name| plugin_name != name);
            }
            sub_index.retain(|_, entries| !entries.is_empty());
        }

        // Shut down the transport
        let plugin = plugin.write().await;
        plugin.transport.shutdown().await?;
        log::info!(target: "plugin", "plugin `{}` stopped", name);
        Ok(())
    }

    /// Stop all running plugins.
    pub async fn shutdown_all(&self) {
        let plugins = self.plugins.read().await;
        for (_, plugin) in plugins.iter() {
            let plugin = plugin.read().await;
            if let Err(e) = plugin.transport.shutdown().await {
                log::warn!(
                    target: "plugin",
                    "error shutting down plugin `{}`: {}",
                    plugin.name, e
                );
            }
        }
    }

    /// List running plugins.
    pub async fn list_plugins(&self) -> Vec<String> {
        let plugins = self.plugins.read().await;
        plugins.keys().cloned().collect()
    }

    /// Execute a hook chain for the given hook point.
    ///
    /// Sends the hook request to each registered plugin in order.
    /// The chain stops on `Complete` or `Reject`. `Continue` passes
    /// to the next plugin (optionally with a modified payload).
    ///
    /// Returns the final `HookResponse` after the chain completes.
    pub async fn run_hook(
        &self,
        hook: &HookPoint,
        payload: serde_json::Value,
    ) -> error::Result<HookResponse> {
        let hook_name = match hook {
            HookPoint::PeerConnected => "peer_connected",
            HookPoint::OpenChannel => "openchannel",
            HookPoint::HtlcAccepted => "htlc_accepted",
            HookPoint::RpcCommand => "rpc_command",
            HookPoint::InvoiceCreation => "invoice_creation",
        };

        // Snapshot the plugin names for this hook
        let plugin_names = {
            let hook_index = self.hook_index.read().await;
            match hook_index.get(hook_name) {
                Some(names) => names.clone(),
                None => return Ok(HookResponse::Continue { payload: None }),
            }
        };

        let plugins = self.plugins.read().await;
        let mut current_payload = payload;

        for plugin_name in &plugin_names {
            let Some(plugin) = plugins.get(plugin_name) else {
                // Plugin was removed between index snapshot and now
                continue;
            };
            let plugin = plugin.read().await;
            if plugin.state != PluginState::Running {
                continue;
            }

            let hook_req = serde_json::json!({
                "method": hook.method_name(),
                "params": current_payload,
                "jsonrpc": "2.0"
            });

            // Send hook request with 30s timeout (hooks are synchronous)
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                plugin.transport.request(hook_req),
            )
            .await;

            match result {
                Ok(Ok(response)) => {
                    // Parse the hook response from the result field
                    let result_value = response
                        .get("result")
                        .cloned()
                        .unwrap_or(serde_json::json!({"result": "continue"}));

                    let hook_resp: HookResponse =
                        serde_json::from_value(result_value).unwrap_or_default();

                    match hook_resp {
                        HookResponse::Continue {
                            payload: Some(modified),
                        } => {
                            // Pass modified payload to next plugin
                            current_payload = modified;
                        }
                        HookResponse::Continue { payload: None } => {
                            // Continue with same payload
                        }
                        HookResponse::Complete { .. } | HookResponse::Reject { .. } => {
                            // Short-circuit the chain
                            log::info!(
                                target: "plugin",
                                "plugin `{}` short-circuited hook `{}`",
                                plugin.name, hook_name
                            );
                            return Ok(hook_resp);
                        }
                    }
                }
                Ok(Err(e)) => {
                    // Transport error — apply failure mode
                    match plugin.manifest.failure_mode {
                        FailureMode::FailOpen => {
                            log::warn!(
                                target: "plugin",
                                "plugin `{}` failed on hook `{}` (fail-open): {}",
                                plugin.name, hook_name, e
                            );
                            continue;
                        }
                        FailureMode::FailClosed => {
                            return Ok(HookResponse::Reject {
                                message: format!(
                                    "plugin `{}` failed on hook `{}`: {}",
                                    plugin.name, hook_name, e
                                ),
                            });
                        }
                    }
                }
                Err(_timeout) => {
                    // Timeout — apply failure mode
                    match plugin.manifest.failure_mode {
                        FailureMode::FailOpen => {
                            log::warn!(
                                target: "plugin",
                                "plugin `{}` timed out on hook `{}` (fail-open)",
                                plugin.name, hook_name
                            );
                            continue;
                        }
                        FailureMode::FailClosed => {
                            return Ok(HookResponse::Reject {
                                message: format!(
                                    "plugin `{}` timed out on hook `{}`",
                                    plugin.name, hook_name
                                ),
                            });
                        }
                    }
                }
            }
        }

        // All plugins returned Continue
        Ok(HookResponse::Continue { payload: None })
    }

    /// Send a notification to all plugins subscribed to the given topic.
    ///
    /// This is fire-and-forget — errors are logged but not propagated.
    pub async fn notify(&self, topic: &str, payload: serde_json::Value) {
        // Snapshot plugin names for this topic
        let mut target_names: Vec<String> = Vec::new();
        {
            let sub_index = self.subscription_index.read().await;
            if let Some(names) = sub_index.get(topic) {
                target_names.extend(names.iter().cloned());
            }
            if let Some(names) = sub_index.get(topics::ALL) {
                target_names.extend(names.iter().cloned());
            }
        }
        // Deduplicate
        target_names.sort_unstable();
        target_names.dedup();

        if target_names.is_empty() {
            return;
        }

        let plugins = self.plugins.read().await;
        let notification = serde_json::json!({
            "method": topic,
            "params": payload,
            "jsonrpc": "2.0"
        });

        for plugin_name in &target_names {
            let Some(plugin) = plugins.get(plugin_name) else {
                continue;
            };
            let plugin = plugin.read().await;
            if plugin.state != PluginState::Running {
                continue;
            }

            if let Err(e) = plugin.transport.notify(notification.clone()).await {
                log::warn!(
                    target: "plugin",
                    "failed to send notification `{}` to plugin `{}`: {}",
                    topic, plugin.name, e
                );
            }
        }
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExternalHandler for PluginManager {
    async fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>> {
        // Look up which plugin owns this method
        let plugin_name = {
            let method_index = self.method_index.read().await;
            match method_index.get(&req.method) {
                Some(name) => name.clone(),
                None => return Ok(None),
            }
        };

        let plugins = self.plugins.read().await;
        let Some(plugin) = plugins.get(&plugin_name) else {
            return Ok(None);
        };

        let plugin = plugin.read().await;
        if plugin.state != PluginState::Running {
            log::warn!(
                target: "plugin",
                "plugin `{}` is not running, skipping method `{}`",
                plugin.name, req.method
            );
            return Ok(None);
        }

        // Build the JSON-RPC request for the plugin
        let plugin_req = serde_json::json!({
            "method": req.method,
            "params": req.params,
            "jsonrpc": "2.0"
        });

        // Forward to the plugin
        let result = plugin.transport.request(plugin_req).await;

        match result {
            Ok(response) => {
                // Extract the result from the JSON-RPC response
                if let Some(error) = response.get("error") {
                    let msg = error
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown plugin error");
                    error::bail!("plugin `{}` error: {}", plugin.name, msg);
                }
                let result = response.get("result").cloned().unwrap_or(json::json!({}));
                Ok(Some(result))
            }
            Err(e) => {
                // Apply failure mode
                match plugin.manifest.failure_mode {
                    FailureMode::FailOpen => {
                        log::warn!(
                            target: "plugin",
                            "plugin `{}` failed (fail-open), skipping: {}",
                            plugin.name, e
                        );
                        Ok(None)
                    }
                    FailureMode::FailClosed => {
                        error::bail!(
                            "plugin `{}` failed (fail-closed): {}",
                            plugin.name,
                            e
                        );
                    }
                }
            }
        }
    }
}
