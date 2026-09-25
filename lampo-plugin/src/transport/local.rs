//! Local plugin transport: spawn the binary, speak gRPC over its Unix socket.
//!
//! This is the stdio replacement. The daemon still owns the process. The
//! plugin listens on `{socket}` instead of reading stdin, so a second
//! `HandleRpc` can run while the first is awaiting a callback.
#![cfg(feature = "grpc")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use lampo_common::error;
use tokio::process::{Child, Command};
use tokio::time::sleep;

use super::grpc::proto::lampo_plugin_client::LampoPluginClient;
use super::uds::UnixConnector;
use super::PluginTransport;

/// A spawned plugin and a cloneable client to its socket.
pub struct LocalGrpcTransport {
    child: tokio::sync::Mutex<Child>,
    client: LampoPluginClient<tonic::transport::Channel>,
    alive: Arc<AtomicBool>,
    addr: String,
}

impl LocalGrpcTransport {
    /// Spawn `plugin_path --lampo-listen 127.0.0.1:0`. The plugin prints
    /// `lampo-listen <addr>` and the daemon dials that. Loopback only.
    pub async fn spawn(plugin_path: &str) -> error::Result<Self> {
        let mut child = Command::new(plugin_path)
            .arg("--lampo-listen")
            .arg("127.0.0.1:0")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| error::anyhow!("failed to spawn plugin `{plugin_path}`: {err}"))?;

        let addr = match read_listen_addr(&mut child).await {
            Ok(addr) => addr,
            Err(err) => {
                let _ = child.kill().await;
                return Err(err);
            }
        };

        let endpoint = tonic::transport::Endpoint::try_from(format!("http://{addr}"))
            .map_err(|err| error::anyhow!("bad plugin addr `{addr}`: {err}"))?
            .connect_timeout(Duration::from_secs(2));
        let channel = endpoint.connect().await.map_err(|err| {
            error::anyhow!("failed to connect to plugin `{plugin_path}` at {addr}: {err}")
        })?;
        // The generated client is Clone. Cloning it does not lock the channel.
        let client = LampoPluginClient::new(channel);

        Ok(Self {
            child: tokio::sync::Mutex::new(child),
            client,
            alive: Arc::new(AtomicBool::new(true)),
            addr,
        })
    }

    fn client(&self) -> LampoPluginClient<tonic::transport::Channel> {
        self.client.clone()
    }
}

async fn read_listen_addr(child: &mut Child) -> error::Result<String> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| error::anyhow!("plugin stdout was not piped"))?;
    let mut lines = BufReader::new(stdout).lines();
    for _ in 0..50 {
        if let Some(status) = child.try_wait()? {
            error::bail!("plugin exited {status} before printing its listen address");
        }
        let line = tokio::time::timeout(Duration::from_millis(200), lines.next_line())
            .await
            .ok()
            .transpose()
            .map_err(|err| error::anyhow!("plugin stdout: {err}"))?
            .flatten();
        let Some(line) = line else {
            continue;
        };
        if let Some(addr) = line.trim().strip_prefix("lampo-listen ") {
            return Ok(addr.to_string());
        }
    }
    error::bail!("plugin did not print `lampo-listen` within 5s");
}

async fn wait_for_socket(path: &std::path::Path, child: &mut Child) -> error::Result<bool> {
    for _ in 0..50 {
        if path.exists() {
            if tokio::net::UnixStream::connect(path).await.is_ok() {
                return Ok(true);
            }
        }
        if let Some(status) = child.try_wait()? {
            log::warn!(target: "plugin", "plugin exited {status} before the socket was ready");
            return Ok(false);
        }
        sleep(Duration::from_millis(100)).await;
    }
    error::bail!(
        "plugin socket `{}` did not accept within 5s",
        path.display()
    );
}

#[async_trait]
impl PluginTransport for LocalGrpcTransport {
    async fn request(&self, msg: serde_json::Value) -> error::Result<serde_json::Value> {
        use super::grpc::proto::{HookRequest, InitRequest, ManifestRequest, RpcRequest};

        let method = msg
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let params = msg.get("params").cloned().unwrap_or(serde_json::json!({}));
        let req_id = msg.get("id").cloned().unwrap_or(serde_json::json!(null));
        let mut client = self.client();

        let response = match method.as_str() {
            "getmanifest" => {
                let resp = client
                    .get_manifest(ManifestRequest {})
                    .await
                    .map_err(|err| error::anyhow!("getmanifest gRPC error: {err}"))?;
                let manifest: serde_json::Value =
                    serde_json::from_str(&resp.into_inner().manifest_json)
                        .map_err(|err| error::anyhow!("invalid manifest JSON: {err}"))?;
                serde_json::json!({"jsonrpc": "2.0", "id": req_id, "result": manifest})
            }
            "init" => {
                let resp = client
                    .init(InitRequest {
                        config_json: serde_json::to_string(&params)?,
                    })
                    .await
                    .map_err(|err| error::anyhow!("init gRPC error: {err}"))?;
                let inner = resp.into_inner();
                let result = if inner.disable_message.is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::json!({"disable": inner.disable_message})
                };
                serde_json::json!({"jsonrpc": "2.0", "id": req_id, "result": result})
            }
            m if m.starts_with("hook/") => {
                let resp = client
                    .handle_hook(HookRequest {
                        hook_name: method,
                        payload_json: serde_json::to_string(&params)?,
                    })
                    .await
                    .map_err(|err| error::anyhow!("hook gRPC error: {err}"))?;
                let result: serde_json::Value =
                    serde_json::from_str(&resp.into_inner().response_json)
                        .map_err(|err| error::anyhow!("hook response is not JSON: {err}"))?;
                serde_json::json!({"jsonrpc": "2.0", "id": req_id, "result": result})
            }
            _ => {
                let resp = client
                    .handle_rpc(RpcRequest {
                        method,
                        params_json: serde_json::to_string(&params)?,
                    })
                    .await
                    .map_err(|err| error::anyhow!("rpc gRPC error: {err}"))?;
                let inner = resp.into_inner();
                if !inner.error_message.is_empty() {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "error": {"code": inner.error_code, "message": inner.error_message}
                    })
                } else {
                    let result: serde_json::Value = serde_json::from_str(&inner.result_json)
                        .map_err(|err| error::anyhow!("plugin rpc result is not JSON: {err}"))?;
                    serde_json::json!({"jsonrpc": "2.0", "id": req_id, "result": result})
                }
            }
        };
        Ok(response)
    }

    async fn notify(&self, msg: serde_json::Value) -> error::Result<()> {
        use super::grpc::proto::NotifyRequest;
        let topic = msg
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let params = msg.get("params").cloned().unwrap_or(serde_json::json!({}));
        self.client()
            .notify(NotifyRequest {
                topic,
                payload_json: serde_json::to_string(&params)?,
            })
            .await
            .map_err(|err| error::anyhow!("notify gRPC error: {err}"))?;
        Ok(())
    }

    async fn shutdown(&self) -> error::Result<()> {
        use super::grpc::proto::ShutdownRequest;
        let _ = self.client().shutdown(ShutdownRequest {}).await;
        self.alive.store(false, Ordering::Release);
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
}
