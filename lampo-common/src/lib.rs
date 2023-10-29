pub mod backend;
pub mod chacha20;
pub mod conf;
pub mod event;
pub mod handler;
pub mod keymanager;
pub mod keys;
pub mod logger;
pub mod model;
pub mod types;
pub mod wallet;

pub mod ldk {
    pub use lightning::*;
    pub use lightning_invoice as invoice;
}

pub mod error {
    pub use anyhow::*;
}

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
