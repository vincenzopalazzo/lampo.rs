//! Channel filter plugin example.
//!
//! Rejects channel open requests below a minimum funding threshold.
//!
//! Build and run:
//! ```sh
//! cargo build --example channel_filter -p lampo-plugin-sdk
//! lampod --plugin target/debug/examples/channel_filter
//! ```
use lampo_plugin_sdk::{HookResponse, Plugin};
use serde_json::Value;

const MIN_FUNDING_SATS: u64 = 100_000;

async fn on_openchannel(params: Value) -> HookResponse {
    let funding = params
        .get("funding_satoshis")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if funding < MIN_FUNDING_SATS {
        log::info!("rejecting channel: {} sats < {} minimum", funding, MIN_FUNDING_SATS);
        HookResponse::Reject {
            message: format!("channel too small: {} < {}", funding, MIN_FUNDING_SATS),
        }
    } else {
        HookResponse::Continue { payload: None }
    }
}

async fn on_channel_ready(params: Value) {
    let channel_id = params
        .get("channel_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    log::info!("channel ready: {}", channel_id);
}

#[tokio::main]
async fn main() {
    env_logger::init();
    Plugin::new()
        .hook("openchannel", on_openchannel)
        .subscribe("channel_ready", on_channel_ready)
        .dynamic(true)
        .start()
        .await;
}
