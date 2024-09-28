use std::sync::Arc;

use tokio::runtime::Handle;
use tokio::{runtime::Runtime, task::JoinHandle};
use tonic::transport::Error as TonicError;
use vls_proxy::vls_protocol_client::KeysManagerClient;

pub struct VLSKeysManager {
    pub async_runtime: Arc<AsyncRuntime>,
    pub keys_manager: KeysManagerClient,
    pub server_handle: Option<JoinHandle<Result<(), TonicError>>>,
}

impl VLSKeysManager {
    pub fn new(
        async_runtime: Arc<AsyncRuntime>,
        keys_manager: KeysManagerClient,
        server_handle: Option<JoinHandle<Result<(), TonicError>>>,
    ) -> Self {
        VLSKeysManager {
            async_runtime,
            keys_manager,
            server_handle,
        }
    }

    pub fn keys_manager(&self) -> &KeysManagerClient {
        &self.keys_manager
    }

    pub fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.async_runtime.block_on(future)
    }
}

pub struct AsyncRuntime {
    runtime: Arc<Runtime>,
}

impl AsyncRuntime {
    pub fn new() -> Self {
        let runtime = Arc::new(Runtime::new().expect("Failed to create Tokio runtime"));
        Self { runtime }
    }

    pub fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.runtime.spawn(future)
    }

    pub fn handle(&self) -> &Handle {
        self.runtime.handle()
    }
}

impl Clone for AsyncRuntime {
    fn clone(&self) -> Self {
        Self {
            runtime: self.runtime.clone(),
        }
    }
}
