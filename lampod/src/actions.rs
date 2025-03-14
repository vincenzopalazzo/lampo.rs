//! Actions crate implementation
pub mod handler;

use async_trait::async_trait;
use lampo_common::chan;
use lampo_common::error;
use lampo_common::ldk::events::Event;

use crate::command::Command;

#[async_trait]
pub trait Handler {
    async fn handle(&self, event: Event) -> error::Result<()>;

    async fn react(&self, event: Command) -> error::Result<()>;
}

pub trait EventHandler: Sized + Send + Sync + Clone {
    fn events(&self) -> chan::Receiver<Event>;
}
