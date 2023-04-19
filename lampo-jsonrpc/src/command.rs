//! Command definition
pub trait Context: Send + Sync {
    type Ctx;

    fn ctx(&self) -> &Self::Ctx;
}
