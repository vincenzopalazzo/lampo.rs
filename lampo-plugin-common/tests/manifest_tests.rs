//! Tests for plugin protocol types serialization.
use lampo_plugin_common::manifest::*;
use lampo_plugin_common::messages::*;

#[test]
fn test_manifest_roundtrip() {
    let manifest = PluginManifest {
        rpc_methods: vec![RpcMethodDecl {
            name: "hello".to_string(),
            description: "Says hello".to_string(),
            usage: "[name]".to_string(),
        }],
        subscriptions: vec!["channel_ready".to_string()],
        hooks: vec![HookDecl {
            name: "openchannel".to_string(),
            before: vec![],
            after: vec![],
        }],
        options: vec![PluginOption {
            name: "greeting".to_string(),
            opt_type: OptionType::String,
            default: Some(serde_json::json!("Hello")),
            description: "The greeting".to_string(),
        }],
        dynamic: true,
        failure_mode: FailureMode::FailOpen,
    };

    let json = serde_json::to_string(&manifest).unwrap();
    let deserialized: PluginManifest = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.rpc_methods.len(), 1);
    assert_eq!(deserialized.rpc_methods[0].name, "hello");
    assert_eq!(deserialized.subscriptions.len(), 1);
    assert_eq!(deserialized.hooks.len(), 1);
    assert!(deserialized.dynamic);
}

#[test]
fn test_hook_response_continue() {
    let resp = HookResponse::Continue { payload: None };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("continue"));

    let deserialized: HookResponse = serde_json::from_str(&json).unwrap();
    matches!(deserialized, HookResponse::Continue { payload: None });
}

#[test]
fn test_hook_response_reject() {
    let resp = HookResponse::Reject {
        message: "too small".to_string(),
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("reject"));
    assert!(json.contains("too small"));
}

#[test]
fn test_init_config_serialization() {
    let config = InitConfig {
        lampo_dir: "/home/user/.lampo/testnet".to_string(),
        network: "testnet".to_string(),
        node_id: "02abcdef".to_string(),
        options: serde_json::Map::new(),
    };
    let json = serde_json::to_string(&config).unwrap();
    assert!(json.contains("testnet"));
    assert!(json.contains("02abcdef"));
}

#[test]
fn test_init_response_disable() {
    let json = r#"{"disable":"not compatible with this network"}"#;
    let resp: InitResponse = serde_json::from_str(json).unwrap();
    assert!(resp.is_disabled());
    assert_eq!(
        resp.disable.unwrap(),
        "not compatible with this network"
    );
}

#[test]
fn test_init_response_ok() {
    let json = r#"{}"#;
    let resp: InitResponse = serde_json::from_str(json).unwrap();
    assert!(!resp.is_disabled());
}

#[test]
fn test_plugin_notification() {
    let notif = PluginNotification::new("channel_ready", serde_json::json!({"channel_id": "abc"}));
    let json = serde_json::to_string(&notif).unwrap();
    assert!(json.contains("channel_ready"));
    assert!(!json.contains("\"id\"")); // notifications have no id
}

#[test]
fn test_empty_manifest_deserialize() {
    let json = "{}";
    let manifest: PluginManifest = serde_json::from_str(json).unwrap();
    assert!(manifest.rpc_methods.is_empty());
    assert!(manifest.hooks.is_empty());
    assert!(manifest.subscriptions.is_empty());
    assert!(!manifest.dynamic);
}
