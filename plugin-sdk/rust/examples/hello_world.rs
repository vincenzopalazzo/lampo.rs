//! Hello World plugin example.
//!
//! Registers a single RPC method `hello` that returns a greeting.
//!
//! Build and run:
//! ```sh
//! cargo build --example hello_world -p lampo-plugin-sdk
//! lampod --plugin target/debug/examples/hello_world
//! ```
use lampo_plugin_sdk::Plugin;
use serde_json::Value;

async fn handle_hello(params: Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("world");
    Ok(serde_json::json!({"message": format!("hello, {}!", name)}))
}

#[tokio::main]
async fn main() {
    env_logger::init();
    Plugin::new()
        .rpc_method("hello", "Says hello", "[name]", handle_hello)
        .dynamic(true)
        .start()
        .await;
}
