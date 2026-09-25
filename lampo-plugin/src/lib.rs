//! Lampo Plugin Infrastructure
//!
//! Daemon-side plugin management: transport layer, lifecycle,
//! and integration with the lampo handler chain.
pub mod manager;
#[cfg(feature = "grpc")]
pub mod tls;
pub mod transport;

pub use manager::PluginManager;
