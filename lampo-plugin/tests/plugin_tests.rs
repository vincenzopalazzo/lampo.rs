//! Integration tests for the lampo plugin system.
use lampo_plugin::manager::PluginManager;
use lampo_plugin::transport::stdio::StdioTransport;
use lampo_plugin::transport::PluginTransport;
use lampo_plugin_common::hooks::HookPoint;
use lampo_plugin_common::messages::{HookResponse, InitConfig};

fn mock_plugin_path() -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    format!("{}/tests/mock_plugin.sh", manifest_dir)
}

fn test_init_config() -> InitConfig {
    InitConfig {
        lampo_dir: "/tmp/lampo-test".to_string(),
        network: "regtest".to_string(),
        node_id: "02abc123".to_string(),
        options: serde_json::Map::new(),
    }
}

#[tokio::test]
async fn test_stdio_transport_lifecycle() {
    let path = mock_plugin_path();
    let transport = StdioTransport::new(&path).await.unwrap();
    assert!(transport.is_alive());

    // Send getmanifest
    let req = serde_json::json!({
        "method": "getmanifest",
        "params": {},
        "jsonrpc": "2.0"
    });
    let resp = transport.request(req).await.unwrap();
    assert!(resp.get("result").is_some());

    let result = resp.get("result").unwrap();
    let methods = result.get("rpc_methods").unwrap().as_array().unwrap();
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0].get("name").unwrap().as_str().unwrap(), "hello");

    // Send init
    let init_req = serde_json::json!({
        "method": "init",
        "params": {
            "lampo_dir": "/tmp/test",
            "network": "regtest",
            "node_id": "",
            "options": {}
        },
        "jsonrpc": "2.0"
    });
    let init_resp = transport.request(init_req).await.unwrap();
    assert!(init_resp.get("result").is_some());

    // Call the hello method
    let hello_req = serde_json::json!({
        "method": "hello",
        "params": {},
        "jsonrpc": "2.0"
    });
    let hello_resp = transport.request(hello_req).await.unwrap();
    let result = hello_resp.get("result").unwrap();
    assert_eq!(
        result.get("message").unwrap().as_str().unwrap(),
        "hello from plugin!"
    );

    // Shutdown
    transport.shutdown().await.unwrap();
    assert!(!transport.is_alive());
}

#[tokio::test]
async fn test_plugin_manager_start_and_route() {
    let path = mock_plugin_path();
    let manager = PluginManager::new();
    let config = test_init_config();

    // Start the mock plugin
    let name = manager.start_plugin(&path, &config).await.unwrap();
    assert_eq!(name, "mock_plugin.sh");

    // Verify plugin is listed
    let plugins = manager.list_plugins().await;
    assert_eq!(plugins.len(), 1);
    assert!(plugins.contains(&"mock_plugin.sh".to_string()));

    // Route an RPC call through ExternalHandler
    use lampo_common::handler::ExternalHandler;
    let req = lampo_common::jsonrpc::Request::new("hello", serde_json::json!({}));
    let result = manager.handle(&req).await.unwrap();
    assert!(result.is_some());
    let result = result.unwrap();
    assert_eq!(
        result.get("message").unwrap().as_str().unwrap(),
        "hello from plugin!"
    );

    // Unknown method should return None (not handled)
    let unknown_req = lampo_common::jsonrpc::Request::new("unknown_method", serde_json::json!({}));
    let result = manager.handle(&unknown_req).await.unwrap();
    assert!(result.is_none());

    // Shutdown
    manager.shutdown_all().await;
}

#[tokio::test]
async fn test_method_collision_rejected() {
    let path = mock_plugin_path();
    let manager = PluginManager::new();
    let config = test_init_config();

    // Start first plugin
    manager.start_plugin(&path, &config).await.unwrap();

    // Starting the same plugin again should fail (method collision)
    let result = manager.start_plugin(&path, &config).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("already registered"));

    manager.shutdown_all().await;
}

#[tokio::test]
async fn test_plugin_stop() {
    let path = mock_plugin_path();
    let manager = PluginManager::new();
    let config = test_init_config();

    let name = manager.start_plugin(&path, &config).await.unwrap();
    assert_eq!(manager.list_plugins().await.len(), 1);

    manager.stop_plugin(&name).await.unwrap();
    assert_eq!(manager.list_plugins().await.len(), 0);

    // Method should no longer route
    use lampo_common::handler::ExternalHandler;
    let req = lampo_common::jsonrpc::Request::new("hello", serde_json::json!({}));
    let result = manager.handle(&req).await.unwrap();
    assert!(result.is_none());
}

fn mock_hooks_plugin_path() -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    format!("{}/tests/mock_plugin_hooks.sh", manifest_dir)
}

#[tokio::test]
async fn test_hook_continue() {
    let path = mock_hooks_plugin_path();
    let manager = PluginManager::new();
    let config = test_init_config();

    manager.start_plugin(&path, &config).await.unwrap();

    // Large channel should pass (continue)
    let payload = serde_json::json!({
        "funding_satoshis": 100000,
        "counterparty_node_id": "02abc",
        "temporary_channel_id": "deadbeef",
    });
    let result = manager.run_hook(&HookPoint::OpenChannel, payload).await.unwrap();
    assert!(matches!(result, HookResponse::Continue { .. }));

    manager.shutdown_all().await;
}

#[tokio::test]
async fn test_hook_reject() {
    let path = mock_hooks_plugin_path();
    let manager = PluginManager::new();
    let config = test_init_config();

    manager.start_plugin(&path, &config).await.unwrap();

    // Small channel should be rejected by the hook plugin
    let payload = serde_json::json!({
        "funding_satoshis": 1000,
        "counterparty_node_id": "02abc",
        "temporary_channel_id": "deadbeef",
    });
    let result = manager.run_hook(&HookPoint::OpenChannel, payload).await.unwrap();
    match result {
        HookResponse::Reject { message } => {
            assert_eq!(message, "channel too small");
        }
        other => panic!("expected Reject, got {:?}", other),
    }

    manager.shutdown_all().await;
}

#[tokio::test]
async fn test_hook_no_plugins_registered() {
    let path = mock_plugin_path(); // basic plugin has no hooks
    let manager = PluginManager::new();
    let config = test_init_config();

    manager.start_plugin(&path, &config).await.unwrap();

    // No plugin registered for openchannel hook — should get Continue
    let payload = serde_json::json!({"funding_satoshis": 1000});
    let result = manager.run_hook(&HookPoint::OpenChannel, payload).await.unwrap();
    assert!(matches!(result, HookResponse::Continue { payload: None }));

    manager.shutdown_all().await;
}

#[tokio::test]
async fn test_notification_does_not_crash() {
    let path = mock_hooks_plugin_path();
    let manager = PluginManager::new();
    let config = test_init_config();

    manager.start_plugin(&path, &config).await.unwrap();

    // Send a notification the plugin is subscribed to
    manager
        .notify(
            "channel_ready",
            serde_json::json!({
                "channel_id": "abc123",
                "counterparty_node_id": "02def",
            }),
        )
        .await;

    // Send a notification the plugin is NOT subscribed to (should be a no-op)
    manager
        .notify(
            "payment",
            serde_json::json!({"state": "success"}),
        )
        .await;

    // Plugin should still be alive and responsive after notifications
    use lampo_common::handler::ExternalHandler;
    let req = lampo_common::jsonrpc::Request::new("greet", serde_json::json!({}));
    let result = manager.handle(&req).await.unwrap();
    assert!(result.is_some());
    assert_eq!(
        result.unwrap().get("greeting").unwrap().as_str().unwrap(),
        "hi there!"
    );

    manager.shutdown_all().await;
}

#[tokio::test]
async fn test_two_plugins_different_methods() {
    let path1 = mock_plugin_path(); // registers "hello"
    let path2 = mock_hooks_plugin_path(); // registers "greet"
    let manager = PluginManager::new();
    let config = test_init_config();

    manager.start_plugin(&path1, &config).await.unwrap();
    manager.start_plugin(&path2, &config).await.unwrap();

    assert_eq!(manager.list_plugins().await.len(), 2);

    // Both methods should route correctly
    use lampo_common::handler::ExternalHandler;
    let hello_req = lampo_common::jsonrpc::Request::new("hello", serde_json::json!({}));
    let result = manager.handle(&hello_req).await.unwrap().unwrap();
    assert_eq!(result.get("message").unwrap().as_str().unwrap(), "hello from plugin!");

    let greet_req = lampo_common::jsonrpc::Request::new("greet", serde_json::json!({}));
    let result = manager.handle(&greet_req).await.unwrap().unwrap();
    assert_eq!(result.get("greeting").unwrap().as_str().unwrap(), "hi there!");

    manager.shutdown_all().await;
}

#[cfg(feature = "grpc")]
#[test]
fn test_tls_cert_generation() {
    use lampo_plugin::tls::CertStore;

    let dir = tempfile::tempdir().unwrap();
    let store = CertStore::new(dir.path().to_str().unwrap());

    // First call generates CA + client certs
    store.ensure_initialized().unwrap();

    assert!(store.ca_cert_path().exists());
    assert!(store.ca_key_path().exists());
    assert!(store.client_cert_path().exists());
    assert!(store.client_key_path().exists());

    // PEM strings should be readable
    let ca_pem = store.ca_cert_pem().unwrap();
    assert!(ca_pem.contains("BEGIN CERTIFICATE"));

    let client_pem = store.client_cert_pem().unwrap();
    assert!(client_pem.contains("BEGIN CERTIFICATE"));

    let client_key = store.client_key_pem().unwrap();
    assert!(client_key.contains("PRIVATE KEY"));

    // Second call should be a no-op (idempotent)
    store.ensure_initialized().unwrap();
}
