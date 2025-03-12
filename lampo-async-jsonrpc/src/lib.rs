//! Full feature async JSON RPC 2.0 Server/client with a
//! minimal dependencies footprint.
use std::future::Future;
use std::sync::Arc;

use jsonrpsee::server::{RpcModule, RpcServiceBuilder, Server, ServerHandle};
use tokio::runtime::Runtime;

pub use jsonrpsee::types::{ErrorObject, ResponsePayload};
pub use jsonrpsee::IntoResponse;

/// JSONRPC v2
pub struct JSONRPCv2<T: Sync + Send + 'static> {
    inner: RpcModule<Arc<T>>,
    host: String,
}

impl<T: Sync + Send + 'static> JSONRPCv2<T> {
    pub fn new(ctx: Arc<T>, host: &str) -> anyhow::Result<Self> {
        Ok(Self {
            inner: RpcModule::new(ctx),
            host: host.to_owned(),
        })
    }

    pub fn add_rpc<R, Fun, Fut>(&mut self, name: &'static str, callback: Fun) -> anyhow::Result<()>
    where
        R: IntoResponse + 'static,
        Fut: Future<Output = R> + Send,
        Fun: (Fn(Arc<T>, serde_json::Value) -> Fut) + Clone + Send + Sync + 'static,
    {
        self.inner.register_async_method(name, move |params, ctx| {
            let request: serde_json::Value = params.parse().unwrap();
            callback(ctx.as_ref().clone(), request)
        })?;
        Ok(())
    }

    pub async fn listen(self) -> std::io::Result<ServerHandle> {
        let rpc_middleware = RpcServiceBuilder::new().rpc_logger(1024);
        let server = Server::builder()
            .set_rpc_middleware(rpc_middleware)
            .build(self.host)
            .await?;
        let handle = server.start(self.inner);
        tokio::spawn(handle.clone().stopped());
        Ok(handle)
    }

    /// Spawing the JSON RPC server on a new thread and a
    /// personal runtime to handle specific RPC call.
    pub fn spawn(self) -> anyhow::Result<()> {
        std::thread::spawn(move || {
            // We should create a single runtime for the JSON RPC server.
            let rt = Runtime::new().unwrap();
            rt.spawn(self.listen())
        });
        // FIXME: return the handler, so we should use a channel a some point.
        Ok(())
    }

    // FIXME: add `spawn_with_runtime` if necessary.
}
