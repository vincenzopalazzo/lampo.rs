//! Actions crate implementation
pub mod handler;

use lightning::util::events::Event;

use lampo_common::error;

use crate::events::LampoEvent;

pub trait Handler {
    fn handle(&self, event: Event) -> error::Result<()>;

    async fn react(&self, event: LampoEvent) -> error::Result<()>;
}
