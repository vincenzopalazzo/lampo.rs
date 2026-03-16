//! gRPC transport for remote plugins.
//!
//! The daemon connects as a gRPC client to a remote plugin server.
//! This mirrors the stdio transport semantics: daemon drives the lifecycle
//! (getmanifest → init → rpc/hook/notify → shutdown).
//!
//! Optional mTLS: the daemon presents a client certificate and verifies
//! the plugin's server certificate against a shared CA.
#![cfg(feature = "grpc")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use lampo_common::error;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};

use super::PluginTransport;

/// Generated gRPC client stubs.
pub mod proto {
    tonic::include_proto!("lampo.plugin.v1");
}

use proto::lampo_plugin_client::LampoPluginClient;

/// Configuration for connecting to a remote plugin.
pub struct GrpcConfig {
    /// The endpoint URL (e.g. "https://host:port").
    pub endpoint: String,
    /// Optional: CA certificate PEM for verifying the plugin server.
    pub ca_cert_pem: Option<String>,
    /// Optional: Client certificate PEM (daemon identity).
    pub client_cert_pem: Option<String>,
    /// Optional: Client private key PEM.
    pub client_key_pem: Option<String>,
}

/// Transport that communicates with a remote plugin over gRPC.
pub struct GrpcTransport {
    client: tokio::sync::Mutex<LampoPluginClient<Channel>>,
    alive: Arc<AtomicBool>,
    endpoint: String,
}

impl GrpcTransport {
    /// Connect to a remote plugin.
    pub async fn connect(config: GrpcConfig) -> error::Result<Self> {
        let mut endpoint = tonic::transport::Endpoint::from_shared(config.endpoint.clone())
            .map_err(|e| error::anyhow!("invalid endpoint `{}`: {}", config.endpoint, e))?;

        // Configure TLS if certificates are provided
        if let Some(ref ca_pem) = config.ca_cert_pem {
            let mut tls = ClientTlsConfig::new().ca_certificate(Certificate::from_pem(ca_pem));

            if let (Some(ref cert_pem), Some(ref key_pem)) =
                (&config.client_cert_pem, &config.client_key_pem)
            {
                tls = tls.identity(Identity::from_pem(cert_pem, key_pem));
            }

            endpoint = endpoint.tls_config(tls)?;
        }

        let channel = endpoint
            .connect()
            .await
            .map_err(|e| error::anyhow!("failed to connect to `{}`: {}", config.endpoint, e))?;

        let client = LampoPluginClient::new(channel);

        Ok(Self {
            client: tokio::sync::Mutex::new(client),
            alive: Arc::new(AtomicBool::new(true)),
            endpoint: config.endpoint,
        })
    }

    /// Get the endpoint URL.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

#[async_trait]
impl PluginTransport for GrpcTransport {
    async fn request(&self, msg: serde_json::Value) -> error::Result<serde_json::Value> {
        let method = msg
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let params = msg.get("params").cloned().unwrap_or(serde_json::json!({}));
        // Preserve the request id for the response envelope
        let req_id = msg.get("id").cloned().unwrap_or(serde_json::json!(null));

        let mut client = self.client.lock().await;

        match method.as_str() {
            "getmanifest" => {
                let resp = client
                    .get_manifest(proto::ManifestRequest {})
                    .await
                    .map_err(|e| error::anyhow!("getmanifest gRPC error: {}", e))?;
                let manifest_json = resp.into_inner().manifest_json;
                let manifest: serde_json::Value = serde_json::from_str(&manifest_json)
                    .map_err(|e| error::anyhow!("invalid manifest JSON: {}", e))?;
                Ok(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": manifest
                }))
            }
            "init" => {
                let config_json = serde_json::to_string(&params)?;
                let resp = client
                    .init(proto::InitRequest { config_json })
                    .await
                    .map_err(|e| error::anyhow!("init gRPC error: {}", e))?;
                let inner = resp.into_inner();
                let result = if inner.disable_message.is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::json!({"disable": inner.disable_message})
                };
                Ok(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": result
                }))
            }
            m if m.starts_with("hook/") => {
                let resp = client
                    .handle_hook(proto::HookRequest {
                        hook_name: method.clone(),
                        payload_json: serde_json::to_string(&params)?,
                    })
                    .await
                    .map_err(|e| error::anyhow!("hook gRPC error: {}", e))?;
                let response_json = resp.into_inner().response_json;
                let result: serde_json::Value = serde_json::from_str(&response_json)
                    .unwrap_or(serde_json::json!({"result": "continue"}));
                Ok(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": result
                }))
            }
            _ => {
                // Regular RPC method
                let resp = client
                    .handle_rpc(proto::RpcRequest {
                        method: method.clone(),
                        params_json: serde_json::to_string(&params)?,
                    })
                    .await
                    .map_err(|e| error::anyhow!("rpc gRPC error: {}", e))?;
                let inner = resp.into_inner();
                if !inner.error_message.is_empty() {
                    Ok(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "error": {
                            "code": inner.error_code,
                            "message": inner.error_message
                        }
                    }))
                } else {
                    let result: serde_json::Value =
                        serde_json::from_str(&inner.result_json).unwrap_or(serde_json::json!({}));
                    Ok(serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": result
                    }))
                }
            }
        }
    }

    async fn notify(&self, msg: serde_json::Value) -> error::Result<()> {
        let topic = msg
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let params = msg.get("params").cloned().unwrap_or(serde_json::json!({}));

        let mut client = self.client.lock().await;
        client
            .notify(proto::NotifyRequest {
                topic,
                payload_json: serde_json::to_string(&params)?,
            })
            .await
            .map_err(|e| {
                log::warn!(target: "plugin::grpc", "notify error: {}", e);
                error::anyhow!("notify gRPC error: {}", e)
            })?;
        Ok(())
    }

    async fn shutdown(&self) -> error::Result<()> {
        let mut client = self.client.lock().await;
        let _ = client.shutdown(proto::ShutdownRequest {}).await;
        self.alive.store(false, Ordering::Release);
        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
}
