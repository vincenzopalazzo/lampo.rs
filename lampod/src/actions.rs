//! Actions crate implementation
pub mod handler;

use lightning::util::events::Event;

pub trait Handler {
    fn handle(&self, event: Event) -> anyhow::Result<()>;
}
