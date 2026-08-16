//! LND-compatible REST API for lampo.
//!
//! Approach C: proto-derived (`prost`/`pbjson`) types, handwritten Actix
//! routes, TLS + macaroon bakery. Zeus talks to this surface as if it were
//! a remote LND node.

pub mod auth;
pub mod convert;
pub mod routes;
pub mod server;
pub mod tls;

#[cfg(test)]
mod wire_tests;

pub mod lnrpc {
    include!(concat!(env!("OUT_DIR"), "/lnrpc.rs"));
    include!(concat!(env!("OUT_DIR"), "/lnrpc.serde.rs"));
}

pub use auth::{AuthError, MacaroonBakery, Permission};
pub use server::{run, spawn, LndRestConfig};
pub use tls::{TlsError, TlsMaterial};
