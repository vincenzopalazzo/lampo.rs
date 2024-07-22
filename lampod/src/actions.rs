//! Actions crate implementation
pub mod handler;

use lampo_common::chan;
use lampo_common::error;
use lampo_common::ldk::events::Event;

use crate::command::Command;

pub trait Handler {
    fn handle(&self, event: Event) -> error::Result<()>;

    fn react(&self, event: Command) -> error::Result<()>;
}

pub trait EventHandler: Sized + Send + Sync + Clone {
    fn events(&self) -> chan::Receiver<Event>;
}
