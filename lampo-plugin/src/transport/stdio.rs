//! Stdio-based plugin transport.
//!
//! Spawns a plugin as a child process and communicates via
//! newline-delimited JSON-RPC 2.0 over stdin/stdout.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use lampo_common::error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex, RwLock};

use super::PluginTransport;

/// Transport that communicates with a plugin subprocess via stdin/stdout.
pub struct StdioTransport {
    /// Stdin writer for the child process.
    stdin: Mutex<tokio::process::ChildStdin>,
    /// Pending requests waiting for responses, keyed by JSON-RPC id.
    pending: Arc<RwLock<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    /// Next request id counter.
    next_id: AtomicU64,
    /// Whether the plugin process is still running.
    alive: Arc<AtomicBool>,
    /// Handle to the child process (for cleanup on drop).
    child: Arc<Mutex<Child>>,
}

impl StdioTransport {
    /// Spawn a plugin subprocess and set up the transport.
    pub async fn new(plugin_path: &str) -> error::Result<Self> {
        let mut child = Command::new(plugin_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| error::anyhow!("failed to spawn plugin `{}`: {}", plugin_path, e))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| error::anyhow!("failed to capture plugin stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| error::anyhow!("failed to capture plugin stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| error::anyhow!("failed to capture plugin stderr"))?;

        let alive = Arc::new(AtomicBool::new(true));
        let pending: Arc<RwLock<HashMap<u64, oneshot::Sender<serde_json::Value>>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Background task: read stdout and dispatch responses
        let pending_clone = pending.clone();
        let alive_clone = alive.clone();
        let plugin_name = plugin_path.to_string();
        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let line = line.trim().to_string();
                        if line.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<serde_json::Value>(&line) {
                            Ok(msg) => {
                                // Extract the id to find the pending request
                                if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
                                    let mut pending = pending_clone.write().await;
                                    if let Some(sender) = pending.remove(&id) {
                                        let _ = sender.send(msg);
                                    } else {
                                        log::warn!(
                                            target: "plugin",
                                            "plugin `{}`: response for unknown id {}",
                                            plugin_name, id
                                        );
                                    }
                                } else {
                                    // Could be a notification from the plugin
                                    log::debug!(
                                        target: "plugin",
                                        "plugin `{}`: received message without id: {}",
                                        plugin_name, line
                                    );
                                }
                            }
                            Err(e) => {
                                log::warn!(
                                    target: "plugin",
                                    "plugin `{}`: invalid JSON from stdout: {} (line: {})",
                                    plugin_name, e, line
                                );
                            }
                        }
                    }
                    Ok(None) => {
                        // EOF: plugin closed stdout
                        log::info!(
                            target: "plugin",
                            "plugin `{}`: stdout closed",
                            plugin_name
                        );
                        alive_clone.store(false, Ordering::SeqCst);
                        break;
                    }
                    Err(e) => {
                        log::error!(
                            target: "plugin",
                            "plugin `{}`: error reading stdout: {}",
                            plugin_name, e
                        );
                        alive_clone.store(false, Ordering::SeqCst);
                        break;
                    }
                }
            }
        });

        // Background task: log stderr at debug level
        let plugin_name = plugin_path.to_string();
        tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log::debug!(target: "plugin", "plugin `{}` stderr: {}", plugin_name, line);
            }
        });

        Ok(Self {
            stdin: Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            alive,
            child: Arc::new(Mutex::new(child)),
        })
    }

    /// Allocate a unique request id.
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }
}

/// Ensure the child process is reaped if the transport is dropped
/// without an explicit shutdown call.
impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        let child = self.child.clone();
        // Spawn a task to reap the child process so we don't block drop
        tokio::spawn(async move {
            let mut child = child.lock().await;
            // Try to kill; ignore errors (child may have already exited)
            let _ = child.kill().await;
            let _ = child.wait().await;
        });
    }
}

#[async_trait]
impl PluginTransport for StdioTransport {
    async fn request(&self, mut msg: serde_json::Value) -> error::Result<serde_json::Value> {
        if !self.is_alive() {
            error::bail!("plugin is not alive");
        }

        let id = self.next_id();

        // Inject the id into the message
        if let Some(obj) = msg.as_object_mut() {
            obj.insert("id".to_owned(), serde_json::Value::Number(id.into()));
        }

        // Register a pending receiver before sending
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.write().await;
            pending.insert(id, tx);
        }

        // Send the message
        let line = serde_json::to_string(&msg)?;
        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| error::anyhow!("failed to write to plugin stdin: {}", e))?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|e| error::anyhow!("failed to write newline to plugin stdin: {}", e))?;
            stdin
                .flush()
                .await
                .map_err(|e| error::anyhow!("failed to flush plugin stdin: {}", e))?;
        }

        // Wait for the response with a timeout
        let response = tokio::time::timeout(std::time::Duration::from_secs(60), rx).await;

        match response {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => {
                // Channel dropped — clean up pending entry
                let mut pending = self.pending.write().await;
                pending.remove(&id);
                error::bail!("plugin response channel dropped")
            }
            Err(_timeout) => {
                // Timeout — clean up the pending entry to avoid leak
                let mut pending = self.pending.write().await;
                pending.remove(&id);
                error::bail!("plugin request timed out after 60s")
            }
        }
    }

    async fn notify(&self, msg: serde_json::Value) -> error::Result<()> {
        if !self.is_alive() {
            error::bail!("plugin is not alive");
        }

        let line = serde_json::to_string(&msg)?;
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| error::anyhow!("failed to write notification to plugin stdin: {}", e))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| error::anyhow!("failed to write newline: {}", e))?;
        stdin
            .flush()
            .await
            .map_err(|e| error::anyhow!("failed to flush: {}", e))?;
        Ok(())
    }

    async fn shutdown(&self) -> error::Result<()> {
        // Send shutdown notification
        let shutdown_msg = serde_json::json!({
            "method": "shutdown",
            "params": {},
            "jsonrpc": "2.0"
        });
        // Best-effort: plugin may already be dead
        let _ = self.notify(shutdown_msg).await;

        // Give the plugin 5 seconds to exit gracefully
        let mut child = self.child.lock().await;
        let timeout = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;

        match timeout {
            Ok(Ok(status)) => {
                log::info!(target: "plugin", "plugin exited with status: {}", status);
            }
            _ => {
                log::warn!(target: "plugin", "plugin did not exit gracefully, killing");
                let _ = child.kill().await;
            }
        }

        self.alive.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}
