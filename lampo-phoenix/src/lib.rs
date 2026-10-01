//! Client side of the ACINQ Phoenix LSP protocol: bLIPs 34, 36 and 41 plus
//! liquidity ads. Nothing here opens, splices or funds a channel yet.
pub mod handler;
pub mod liquidity_ads;
pub mod policy;
pub mod purchases;
pub mod wire;

pub use handler::PhoenixLspHandler;
pub use purchases::PurchaseStore;
