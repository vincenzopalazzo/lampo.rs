pub mod backend;
pub mod conf;
pub mod event;
pub mod handler;
pub mod jsonrpc;
pub mod keys;
pub mod logger;
pub mod model;
pub mod types;
pub mod utils;
pub mod wallet;

pub mod ldk {
    pub use lightning::bolt11_invoice as invoice;
    pub use lightning::*;
    pub use lightning_background_processor as processor;
    pub use lightning_block_sync as block_sync;
    pub use lightning_net_tokio as net;
    pub use lightning_persister as persister;
}

pub mod error {
    pub use anyhow::*;
}

pub use serde;

pub mod json {
    pub use serde::de::DeserializeOwned;
    pub use serde::{Deserialize, Serialize};
    pub use serde_json::*;

    pub mod prelude {
        pub use serde;
        pub use serde::*;
    }
}

pub mod chan {
    pub use crossbeam_channel::*;
}

pub use async_trait::async_trait;
pub use bitcoin;
pub use bitcoin::secp256k1;

pub mod btc_rpc {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    pub struct MinimumMempoolFee {
        /// Minimum fee rate in BTC/kB for tx to be accepted. Is the maximum of minrelaytxfee and minimum mempool fee
        pub mempoolminfee: f32,
    }
}
