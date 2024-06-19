pub mod backend;
pub mod chacha20;
pub mod conf;
pub mod event;
pub mod handler;
pub mod keys;
pub mod logger;
pub mod model;
pub mod types;
pub mod wallet;

#[cfg(feature = "vanilla")]
pub mod ldk {
    pub use lightning::*;
    pub use lightning_block_sync as sync;
    pub use lightning_background_processor as processor;
    pub use lightning_invoice as invoice;
    pub use lightning_net_tokio as net;
    pub use lightning_persister as persister;
}

#[cfg(feature = "rgb")]
pub mod ldk {
    pub use rgb_lightning::*;
    pub use rgb_lightning_block_sync as sync;
    pub use rgb_lightning_background_processor as processor;
    pub use rgb_lightning_invoice as invoice;
    pub use rgb_lightning_net_tokio as net;
    pub use rgb_lightning_persister as persister;
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

#[cfg(feature = "vanilla")]
pub mod btc {
    pub use bitcoin;
    pub use bitcoin::absolute::Height;
    pub use bitcoin::secp256k1;
    pub use bitcoin::block::Header;
    pub use bitcoin::{ScriptBuf, Transaction};
}

#[cfg(feature = "rgb")]
pub mod btc {
    pub use bitcoin_29 as bitcoin;
    pub use bitcoin_29::secp256k1;
    pub use bitcoin_29::blockdata::locktime::Height;
    pub use bitcoin_29::blockdata::block::BlockHeader as Header;
    pub use bitcoin_29::{Script as ScriptBuf, Transaction};
}

pub mod btc_rpc {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    pub struct MinimumMempoolFee {
        /// Minimum fee rate in BTC/kB for tx to be accepted. Is the maximum of minrelaytxfee and minimum mempool fee
        pub mempoolminfee: f32,
    }
}
