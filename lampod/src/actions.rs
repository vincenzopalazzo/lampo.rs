//! Actions crate implementation
pub mod handler;

use lightning::util::events::Event;

use lampo_common::error;

pub trait Handler {
    fn handle(&self, event: Event) -> error::Result<()>;
}
