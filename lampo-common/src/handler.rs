use crate::error;
use crate::event::Event;
use crate::json;
use crate::jsonrpc::Request;
use tokio::sync::mpsc;

pub trait Handler: Send + Sync {
    fn events(&self) -> mpsc::UnboundedReceiver<Event>;
    fn emit(&self, event: Event);
}

pub trait ExternalHandler {
    fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>>;
}
