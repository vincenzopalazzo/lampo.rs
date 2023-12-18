//! Command Core Definition

/// Context Interface used to pass around any
/// kind of state that the RPC command will use.
pub trait Context: Send + Sync {
    type Ctx;

    fn ctx(&self) -> &Self::Ctx;
}
