//! JSON-RPC I/O loop for plugin lifecycle.
//!
//! Reads newline-delimited JSON-RPC from input, dispatches to handlers,
//! writes responses to output.
use std::collections::HashMap;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use lampo_plugin_common::manifest::PluginManifest;
use serde_json::Value;

use crate::{HookHandler, NotifyHandler, RpcHandler};

pub async fn run_plugin<R, W>(
    input: R,
    mut output: W,
    manifest: PluginManifest,
    rpc_handlers: HashMap<String, RpcHandler>,
    hook_handlers: HashMap<String, HookHandler>,
    notify_handlers: HashMap<String, NotifyHandler>,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let reader = BufReader::new(input);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("invalid JSON: {}", e);
                continue;
            }
        };

        let method = msg
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Object(Default::default()));

        // If no "id", it's a notification (fire-and-forget)
        if id.is_none() {
            if let Some(handler) = notify_handlers.get(&method) {
                handler(params).await;
            }
            continue;
        }

        let id = id.unwrap();

        let response = match method.as_str() {
            "getmanifest" => {
                let manifest_json = serde_json::to_value(&manifest).unwrap_or(Value::Object(Default::default()));
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": manifest_json
                })
            }
            "init" => {
                // Plugin could inspect params for config, for now just acknowledge
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {}
                })
            }
            "shutdown" => {
                // Write ack and exit
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {}
                });
                let mut resp_str = serde_json::to_string(&resp).unwrap();
                resp_str.push('\n');
                let _ = output.write_all(resp_str.as_bytes()).await;
                let _ = output.flush().await;
                return;
            }
            m if m.starts_with("hook/") => {
                // Find hook handler by the hook name (without "hook/" prefix)
                let hook_name = &m["hook/".len()..];
                if let Some(handler) = hook_handlers.get(hook_name) {
                    let hook_resp = handler(params).await;
                    let result = serde_json::to_value(&hook_resp).unwrap_or(serde_json::json!({"result": "continue"}));
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": result
                    })
                } else {
                    // Unknown hook — continue
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {"result": "continue"}
                    })
                }
            }
            _ => {
                // Try RPC handler
                if let Some(handler) = rpc_handlers.get(&method) {
                    match handler(params).await {
                        Ok(result) => {
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": result
                            })
                        }
                        Err(msg) => {
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": {"code": -32000, "message": msg}
                            })
                        }
                    }
                } else {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32601, "message": "method not found"}
                    })
                }
            }
        };

        let mut resp_str = serde_json::to_string(&response).unwrap();
        resp_str.push('\n');
        if let Err(e) = output.write_all(resp_str.as_bytes()).await {
            log::error!("failed to write response: {}", e);
            return;
        }
        if let Err(e) = output.flush().await {
            log::error!("failed to flush output: {}", e);
            return;
        }
    }
}
