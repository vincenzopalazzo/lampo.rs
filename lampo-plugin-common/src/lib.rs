//! Lampo Plugin Protocol Types
//!
//! This crate defines the wire protocol types shared between the lampo daemon
//! and plugins. Plugins can be written in any language — they just need to
//! speak JSON-RPC 2.0 over stdin/stdout (local) or implement the gRPC service
//! (remote).
pub mod hooks;
pub mod manifest;
pub mod messages;
pub mod topics;

pub use manifest::*;
pub use messages::*;
