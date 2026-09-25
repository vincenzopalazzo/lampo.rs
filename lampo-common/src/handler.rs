use crate::async_trait;
use crate::chan;
use crate::error;
use crate::event::Event;
use crate::json;
use crate::jsonrpc::Request;

pub trait Handler: Send + Sync {
    fn events(&self) -> chan::UnboundedReceiver<Event>;
    fn emit(&self, event: Event);

    /// Dispatch a method through the external-handler chain.
    ///
    /// Chain sync uses this for the bitcoind plugin (`bitcoind`) instead of
    /// holding a `PluginManager`. Default is "not supported" so emitters that
    /// only implement events keep compiling.
    fn call<'a>(
        &'a self,
        method: &'a str,
        args: json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = error::Result<json::Value>> + Send + 'a>>
    {
        let _ = (method, args);
        Box::pin(async move { error::bail!("handler does not dispatch RPC methods") })
    }
}

#[async_trait]
pub trait ExternalHandler: Send + Sync {
    async fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>>;
}
