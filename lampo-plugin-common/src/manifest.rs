//! Plugin manifest types.
//!
//! A plugin declares its capabilities via a `PluginManifest` returned
//! in response to the `getmanifest` JSON-RPC call from the daemon.
use serde::{Deserialize, Serialize};

/// The full manifest a plugin returns to declare its capabilities.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginManifest {
    /// Custom RPC methods this plugin provides.
    #[serde(default)]
    pub rpc_methods: Vec<RpcMethodDecl>,
    /// Event topics this plugin subscribes to (e.g. "channel_ready", "payment").
    #[serde(default)]
    pub subscriptions: Vec<String>,
    /// Hooks this plugin wants to intercept.
    #[serde(default)]
    pub hooks: Vec<HookDecl>,
    /// Configuration options the plugin declares.
    #[serde(default)]
    pub options: Vec<PluginOption>,
    /// Whether this plugin can be started/stopped dynamically at runtime.
    #[serde(default)]
    pub dynamic: bool,
    /// What to do when this plugin is unreachable.
    #[serde(default)]
    pub failure_mode: FailureMode,
}

/// Declaration of an RPC method a plugin provides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcMethodDecl {
    /// The method name (e.g. "hello", "mycommand").
    pub name: String,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Usage string (e.g. "[name] [amount]").
    #[serde(default)]
    pub usage: String,
}

/// Declaration of a hook a plugin wants to intercept.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookDecl {
    /// The hook name (e.g. "openchannel", "htlc_accepted").
    pub name: String,
    /// This plugin should run before these plugins.
    #[serde(default)]
    pub before: Vec<String>,
    /// This plugin should run after these plugins.
    #[serde(default)]
    pub after: Vec<String>,
}

/// A configuration option the plugin declares.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginOption {
    /// Option name.
    pub name: String,
    /// Option type.
    #[serde(rename = "type")]
    pub opt_type: OptionType,
    /// Default value (if any).
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
}

/// Supported option types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OptionType {
    String,
    Int,
    Bool,
}

/// What to do when a plugin is unreachable or times out.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FailureMode {
    /// Skip the plugin and continue (non-critical plugins).
    #[default]
    FailOpen,
    /// Fail the operation if the plugin is down (critical plugins).
    FailClosed,
}
