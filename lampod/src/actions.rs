//! Actions crate implementation
pub mod handler;

use lightning::events::Event;

use lampo_common::error;

use crate::events::{InventoryEvent, LampoEvent};

pub trait Handler {
    fn handle(&self, event: Event) -> error::Result<()>;

    fn react(&self, event: LampoEvent) -> error::Result<()>;
}

/// The Handler that need to implement for handle
/// inventory event
///
/// This is necessary because ldk does not have any
/// concept of Inventory Manager.
pub trait InventoryHandler {
    fn handle(&self, event: InventoryEvent) -> error::Result<()>;
}
