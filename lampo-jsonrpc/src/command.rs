//! Command definition
pub trait Context: Send + Sync {
    type Ctx;

    fn ctx(&mut self) -> &mut Self::Ctx;
}
