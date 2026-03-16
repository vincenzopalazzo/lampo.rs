//! Integration tests for the plugin SDK.
//!
//! Tests the full lifecycle using mock I/O (no real stdin/stdout).
use lampo_plugin_sdk::{HookResponse, Plugin};
use serde_json::Value;
use tokio::io::{duplex, AsyncWriteExt, BufReader, AsyncBufReadExt};

/// Helper: send a JSON-RPC request and read the response.
async fn roundtrip(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
    method: &str,
    params: Value,
    id: u64,
) -> Value {
    let req = serde_json::json!({
        "method": method,
        "params": params,
        "id": id,
        "jsonrpc": "2.0"
    });
    let mut line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).await.unwrap();
    writer.flush().await.unwrap();

    let mut response_line = String::new();
    reader.read_line(&mut response_line).await.unwrap();
    serde_json::from_str(&response_line).unwrap()
}

#[tokio::test]
async fn test_sdk_full_lifecycle() {
    // Create duplex streams for mock I/O
    let (plugin_input, mut test_writer) = duplex(4096);
    let (mut plugin_output_reader, plugin_output) = duplex(4096);

    let plugin = Plugin::new()
        .rpc_method("hello", "Says hello", "[name]", |_params| async {
            Ok(serde_json::json!({"message": "hello from sdk!"}))
        })
        .hook("openchannel", |params| async move {
            let funding = params.get("funding_satoshis")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if funding < 10000 {
                HookResponse::Reject { message: "too small".to_string() }
            } else {
                HookResponse::Continue { payload: None }
            }
        })
        .subscribe("channel_ready", |_params| async {
            // Just absorb the notification
        });

    // Run plugin in background
    let handle = tokio::spawn(async move {
        plugin.run(plugin_input, plugin_output).await;
    });

    let mut reader = BufReader::new(&mut plugin_output_reader);

    // 1. getmanifest
    let resp = roundtrip(&mut test_writer, &mut reader, "getmanifest", serde_json::json!({}), 1).await;
    let result = resp.get("result").unwrap();
    let methods = result.get("rpc_methods").unwrap().as_array().unwrap();
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0]["name"].as_str().unwrap(), "hello");
    let hooks = result.get("hooks").unwrap().as_array().unwrap();
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0]["name"].as_str().unwrap(), "openchannel");
    let subs = result.get("subscriptions").unwrap().as_array().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].as_str().unwrap(), "channel_ready");

    // 2. init
    let resp = roundtrip(&mut test_writer, &mut reader, "init", serde_json::json!({
        "lampo_dir": "/tmp/test",
        "network": "regtest",
        "node_id": "",
        "options": {}
    }), 2).await;
    assert!(resp.get("result").is_some());

    // 3. RPC call
    let resp = roundtrip(&mut test_writer, &mut reader, "hello", serde_json::json!({}), 3).await;
    assert_eq!(
        resp["result"]["message"].as_str().unwrap(),
        "hello from sdk!"
    );

    // 4. Hook — large channel (continue)
    let resp = roundtrip(&mut test_writer, &mut reader, "hook/openchannel", serde_json::json!({
        "funding_satoshis": 100000
    }), 4).await;
    let hook_result = resp["result"]["result"].as_str().unwrap();
    assert_eq!(hook_result, "continue");

    // 5. Hook — small channel (reject)
    let resp = roundtrip(&mut test_writer, &mut reader, "hook/openchannel", serde_json::json!({
        "funding_satoshis": 5000
    }), 5).await;
    assert_eq!(resp["result"]["result"].as_str().unwrap(), "reject");
    assert_eq!(resp["result"]["message"].as_str().unwrap(), "too small");

    // 6. Notification (no id, no response expected)
    let notif = serde_json::json!({
        "method": "channel_ready",
        "params": {"channel_id": "abc"},
        "jsonrpc": "2.0"
    });
    let mut notif_str = serde_json::to_string(&notif).unwrap();
    notif_str.push('\n');
    test_writer.write_all(notif_str.as_bytes()).await.unwrap();
    test_writer.flush().await.unwrap();

    // 7. Unknown method
    let resp = roundtrip(&mut test_writer, &mut reader, "unknown", serde_json::json!({}), 6).await;
    assert!(resp.get("error").is_some());

    // 8. Shutdown
    let resp = roundtrip(&mut test_writer, &mut reader, "shutdown", serde_json::json!({}), 7).await;
    assert!(resp.get("result").is_some());

    // Plugin should exit
    handle.await.unwrap();
}

#[tokio::test]
async fn test_sdk_rpc_error_handling() {
    let (plugin_input, mut test_writer) = duplex(4096);
    let (mut plugin_output_reader, plugin_output) = duplex(4096);

    let plugin = Plugin::new()
        .rpc_method("fail_me", "Always fails", "", |_| async {
            Err("something went wrong".to_string())
        });

    let handle = tokio::spawn(async move {
        plugin.run(plugin_input, plugin_output).await;
    });

    let mut reader = BufReader::new(&mut plugin_output_reader);

    // Skip getmanifest + init
    let _ = roundtrip(&mut test_writer, &mut reader, "getmanifest", serde_json::json!({}), 1).await;
    let _ = roundtrip(&mut test_writer, &mut reader, "init", serde_json::json!({}), 2).await;

    // Call the failing method
    let resp = roundtrip(&mut test_writer, &mut reader, "fail_me", serde_json::json!({}), 3).await;
    let error = resp.get("error").unwrap();
    assert_eq!(error["message"].as_str().unwrap(), "something went wrong");
    assert_eq!(error["code"].as_i64().unwrap(), -32000);

    // Shutdown
    let _ = roundtrip(&mut test_writer, &mut reader, "shutdown", serde_json::json!({}), 4).await;
    handle.await.unwrap();
}
