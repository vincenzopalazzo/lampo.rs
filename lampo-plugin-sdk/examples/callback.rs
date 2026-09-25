//! Proof that a plugin can call lampo without deadlocking stdin.
//!
//! `whoami` calls `getinfo` on the daemon's `LampoHost` at `127.0.0.1`.
//! That is a second gRPC connection. If this returned, the callback did
//! not go back down the plugin's own `HandleRpc`.
//!
//! ```sh
//! cargo build -p lampo-plugin-sdk --example callback
//! lampod-cli --plugin target/debug/examples/callback
//! lampo-cli whoami
//! ```
use std::sync::Arc;

use lampo_plugin::transport::grpc::proto::lampo_host_client::LampoHostClient;
use lampo_plugin::transport::grpc::proto::RpcRequest;
use lampo_plugin_sdk::Plugin;
use serde_json::{json, Value};
use tokio::sync::RwLock;

async fn lampo_call(addr: &str, method: &str) -> Result<Value, String> {
    let mut client = LampoHostClient::connect(format!("http://{addr}"))
        .await
        .map_err(|err| format!("connect `{addr}`: {err}"))?;
    let response = client
        .call(RpcRequest {
            method: method.to_string(),
            params_json: "{}".to_string(),
        })
        .await
        .map_err(|err| format!("call `{method}`: {err}"))?
        .into_inner();
    if !response.error_message.is_empty() {
        return Err(response.error_message);
    }
    serde_json::from_str(&response.result_json).map_err(|err| format!("bad result: {err}"))
}

#[tokio::main]
async fn main() {
    let rpc_file = Arc::new(RwLock::new(String::new()));
    let rpc_for_init = rpc_file.clone();
    let rpc_for_method = rpc_file.clone();
    Plugin::new()
        .rpc_method(
            "whoami",
            "Call getinfo on lampo-rpc while this request is in flight",
            "",
            move |_params: Value| {
                let rpc_file = rpc_for_method.clone();
                async move {
                    let path = rpc_file.read().await.clone();
                    if path.is_empty() {
                        return Err("init did not set rpc_file".to_owned());
                    }
                    let info = lampo_call(&path, "getinfo").await?;
                    let node_id = info.get("node_id").cloned().unwrap_or(Value::Null);
                    Ok(json!({"via": "grpc", "node_id": node_id}))
                }
            },
        )
        .on_init(move |params: Value| {
            let rpc_file = rpc_for_init.clone();
            async move {
                let path = params
                    .get("rpc_file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                *rpc_file.write().await = path;
                Ok(())
            }
        })
        .start()
        .await;
}
