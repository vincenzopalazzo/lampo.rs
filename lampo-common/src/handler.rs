use async_trait::async_trait;

use crate::chan;
use crate::error;
use crate::event::Event;
use crate::json;

/// Internal Handler Implementation.
///
/// Allow lampo to communica with crate like bitcoin backend
/// or wallets by using events. So in order to listen to lampo
/// events you could use one of the following code example
///
/// ```ingore
/// let events = lampo.events();
/// while let Ok(event) = events.recv_timeout(Duration::from_millis(100)) {
///     let Event::Lightning(LightningEvent::ChannelReady { ..  }) = event else { continue };
///     log::info!(target: "tests", "event received {:?}", event);
/// }
/// ```
pub trait Handler: Send + Sync {
    /// Get a received channel where to receive incoming invent
    /// from the moment of the function call.
    fn events(&self) -> chan::Receiver<Event>;
    /// Be able to regerate an event that will be propagated to all
    /// the events receivers that are living in the program.
    fn emit(&self, event: Event);
}

/// Handler used to communicate with an external source.
///
/// This allow the inprocess call handling to work with external handlers.
#[async_trait]
pub trait ExternalHandler {
    /// React to an external call
    async fn handle(&self, method: &str, body: &json::Value) -> error::Result<Option<json::Value>>;
}
