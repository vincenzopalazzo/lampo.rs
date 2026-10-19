//! Bitcoind chain backend, written as a lampo plugin.
//!
//! The daemon spawns this binary and sends `core-url`, `core-user`, and
//! `core-pass` in `init`. Chain sync then calls the RPC methods below
//! instead of opening the bitcoind port itself.
//!
//! ```text
//! cargo run -p lampo-plugin-sdk --example bitcoind
//! lampod-cli --plugin target/debug/examples/bitcoind
//! ```
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lampo_plugin_sdk::Plugin;
use serde_json::{json, Value};
use tokio::sync::RwLock;

struct Bitcoind {
    endpoint: String,
    basic_auth: String,
    client: reqwest::Client,
    next_id: AtomicU64,
}

impl Bitcoind {
    fn from_init(params: &Value) -> Result<Self, String> {
        let url = params
            .get("options")
            .and_then(|o| o.get("core-url"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or("bitcoind plugin: init missing options.core-url")?;
        let (user, pass) = basic_credentials(params)?;
        Ok(Self {
            endpoint: url.to_owned(),
            basic_auth: format!("Basic {}", basic_auth(&user, &pass)),
            client: reqwest::Client::new(),
            next_id: AtomicU64::new(1),
        })
    }

    async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let response = self
            .client
            .post(&self.endpoint)
            .header("Authorization", &self.basic_auth)
            .json(&body)
            .send()
            .await
            .map_err(|err| format!("bitcoind `{method}` transport error: {err}"))?;
        let bytes = response
            .bytes()
            .await
            .map_err(|err| format!("bitcoind `{method}` empty body: {err}"))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|err| format!("bitcoind `{method}` invalid JSON: {err}"))?;
        if let Some(error) = value.get("error").filter(|err| !err.is_null()) {
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("bitcoind error");
            return Err(format!("bitcoind `{method}`: {message}"));
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| format!("bitcoind `{method}` returned no result"))
    }
}

/// Cookie file wins over `core-user` / `core-pass`, matching the wallet.
fn basic_credentials(params: &Value) -> Result<(String, String), String> {
    if let Some(path) = params
        .pointer("/options/rpc-cookie-file")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let raw = std::fs::read_to_string(path)
            .map_err(|err| format!("bitcoind cookie file `{path}`: {err}"))?;
        let raw = raw.trim_end_matches(['\r', '\n']);
        let (user, pass) = raw
            .split_once(':')
            .ok_or("bitcoind cookie file is not user:password")?;
        if user.is_empty() || pass.is_empty() {
            return Err("bitcoind cookie file is not user:password".to_owned());
        }
        return Ok((user.to_owned(), pass.to_owned()));
    }
    let user = params
        .pointer("/options/core-user")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let pass = params
        .pointer("/options/core-pass")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    Ok((user, pass))
}

fn basic_auth(user: &str, pass: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
}

fn rpc(
    node: Arc<RwLock<Option<Bitcoind>>>,
    method: &'static str,
) -> impl Fn(
    Value,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send>>
       + Send
       + Sync
       + Clone
       + 'static {
    move |params: Value| {
        let node = node.clone();
        let method = method.to_owned();
        Box::pin(async move {
            let args = match params {
                Value::Array(items) => items,
                Value::Object(map) if map.is_empty() => Vec::new(),
                other => {
                    return Err(format!(
                        "bitcoind plugin: `{method}` params must be an array, got {other}"
                    ));
                }
            };
            let guard = node.read().await;
            let rpc = guard.as_ref().ok_or("bitcoind plugin: init has not run")?;
            rpc.call(&method, args).await
        })
    }
}

#[tokio::main]
async fn main() {
    let node = Arc::new(RwLock::new(None));
    let plugin = Plugin::new()
        .failure_mode(lampo_plugin_sdk::FailureMode::FailClosed)
        .rpc_method(
            "sendrawtransaction",
            "Broadcast a raw transaction",
            "hex",
            rpc(node.clone(), "sendrawtransaction"),
        )
        .rpc_method(
            "getblock",
            "Fetch a block by hash",
            "hash verbosity",
            rpc(node.clone(), "getblock"),
        )
        .rpc_method(
            "getblockheader",
            "Fetch a block header by hash",
            "hash",
            rpc(node.clone(), "getblockheader"),
        )
        .rpc_method(
            "getblockchaininfo",
            "Chain tip and height",
            "",
            rpc(node.clone(), "getblockchaininfo"),
        )
        .rpc_method(
            "estimatesmartfee",
            "Fee estimate in BTC/kvB",
            "blocks mode",
            rpc(node.clone(), "estimatesmartfee"),
        )
        .rpc_method(
            "getmempoolinfo",
            "Mempool minimum fee",
            "",
            rpc(node.clone(), "getmempoolinfo"),
        );
    let node_for_init = node.clone();
    plugin
        .on_init(move |params| {
            let node_for_init = node_for_init.clone();
            async move {
                let rpc = Bitcoind::from_init(&params)?;
                *node_for_init.write().await = Some(rpc);
                Ok(())
            }
        })
        .start()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_file_wins_over_user_and_password() {
        let dir = std::env::temp_dir().join(format!("lampo-bitcoind-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cookie = dir.join("rpc.cookie");
        std::fs::write(&cookie, "from-cookie:secret").unwrap();
        let params = json!({
            "options": {
                "rpc-cookie-file": cookie,
                "core-user": "ignored",
                "core-pass": "ignored",
            }
        });
        let (user, pass) = basic_credentials(&params).unwrap();
        assert_eq!(user, "from-cookie");
        assert_eq!(pass, "secret");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
