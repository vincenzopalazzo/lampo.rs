//! Actions crate implementation
pub mod handler;

use async_trait::async_trait;
use crossbeam_channel as chan;

use lampo_common::error;
use lampo_common::ldk::events::Event;

use crate::command::{Command, InventoryCommand};

#[async_trait]
pub trait Handler {
    async fn handle(&self, event: Event) -> error::Result<()>;

    async fn react(&self, event: Command) -> error::Result<()>;
}

pub struct DummyHandler;

// FIXME: move to the inMemory handler
#[async_trait]
impl Handler for DummyHandler {
    async fn handle(&self, _: Event) -> error::Result<()> {
        Ok(())
    }

    async fn react(&self, _: Command) -> error::Result<()> {
        Ok(())
    }
}

/// The Handler that need to implement for handle
/// inventory event
///
/// This is necessary because ldk does not have any
/// concept of Inventory Manager.
pub trait InventoryHandler {
    fn handle(&self, event: InventoryCommand) -> error::Result<()>;
}

pub trait EventHandler: Sized + Send + Sync + Clone {
    fn events(&self) -> chan::Receiver<Event>;
}
