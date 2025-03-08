use crate::chan;
use crate::error;
use crate::event::Event;
use crate::json;
use crate::jsonrpc::Request;

pub trait Handler: Send + Sync {
    fn events(&self) -> chan::Receiver<Event>;
    fn emit(&self, event: Event);
}

pub trait ExternalHandler {
    fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>>;
}
