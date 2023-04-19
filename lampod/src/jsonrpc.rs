//! JSON RPC 2.0 implementation
pub mod inventory;
pub mod peer_control;

use lampo_jsonrpc::command::Context;

use crate::LampoDeamon;

/// Implementing the Context for the JSON RPC 2.0 framework
impl Context for LampoDeamon {
    type Ctx = LampoDeamon;

    fn ctx(&self) -> &Self::Ctx {
        self
    }
}
