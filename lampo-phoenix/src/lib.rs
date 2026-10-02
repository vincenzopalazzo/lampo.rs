//! Client side of the ACINQ Phoenix LSP protocol: bLIPs 34, 36 and 41 plus
//! liquidity ads, packaged as a daemon extension. Nothing here opens,
//! splices or funds a channel yet.
pub mod channels;
pub mod conf;
pub mod events;
pub mod handler;
pub mod liquidity_ads;
pub mod offer;
pub mod policy;
pub mod purchases;
pub mod rpc;
pub mod wire;

pub use handler::PhoenixLspHandler;
pub use purchases::PurchaseStore;
