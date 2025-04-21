use crate::async_trait;
use crate::chan;
use crate::error;
use crate::event::Event;
use crate::json;
use crate::jsonrpc::Request;

pub trait Handler: Send + Sync {
    fn events(&self) -> chan::UnboundedReceiver<Event>;
    fn emit(&self, event: Event);
}

#[async_trait]
pub trait ExternalHandler: Send + Sync {
    async fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>>;
}
