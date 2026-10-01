//! Proof that one plugin method can call another, and that method can
//! call back into lampo, without sharing a pipe.
//!
//! `foo` is in flight. It calls `yoooo` through `LampoHost` on
//! `127.0.0.1`. `yoooo` is a second `HandleRpc`, and it calls `getinfo`
//! the same way. If `foo` returns `via: grpc`, both calls completed
//! while the first handler was still waiting.
//!
//! ```sh
//! cargo build -p lampo-plugin-sdk --example reentry
//! lampod-cli --plugin target/debug/examples/reentry
//! lampo-cli foo
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
    let rpc_for_foo = rpc_file.clone();
    let rpc_for_yoooo = rpc_file.clone();
    Plugin::new()
        .rpc_method(
            "foo",
            "Call yoooo on this plugin while foo is in flight",
            "",
            move |_params: Value| {
                let rpc_file = rpc_for_foo.clone();
                async move {
                    let path = rpc_file.read().await.clone();
                    if path.is_empty() {
                        return Err("init did not set rpc_file".to_owned());
                    }
                    let result = lampo_call(&path, "yoooo").await?;
                    Ok(json!({"called": "yoooo", "yoooo": result}))
                }
            },
        )
        .rpc_method(
            "yoooo",
            "Call getinfo while foo is still waiting",
            "",
            move |_params: Value| {
                let rpc_file = rpc_for_yoooo.clone();
                async move {
                    let path = rpc_file.read().await.clone();
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
