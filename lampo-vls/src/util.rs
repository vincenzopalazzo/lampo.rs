use std::sync::Arc;

use triggered::{Trigger, Listener};
use vls_proxy::vls_protocol_client::KeysManagerClient;
use tokio::{runtime::Runtime, task::JoinHandle};
use tonic::transport::Error as TonicError;
use tokio::runtime::Handle;


#[derive(Clone)]
pub struct Shutter {
	pub trigger: Trigger,
	pub signal: Listener,
}

impl Shutter {
	pub fn new() -> Self {
		let (trigger, signal) = triggered::trigger();
		let ctrlc_trigger = trigger.clone();
		ctrlc::set_handler(move || {
			ctrlc_trigger.trigger();
		})
		.expect("Error setting Ctrl-C handler - do you have more than one?");

		Self { trigger, signal }
	}
}


pub struct VLSKeysManager {
    pub async_runtime: Arc<AsyncRuntime>,
    pub keys_manager: KeysManagerClient,
    pub _server_handle: Option<JoinHandle<Result<(), TonicError>>>
}

impl VLSKeysManager {
    pub fn new(async_runtime: Arc<AsyncRuntime>, keys_manager: KeysManagerClient, server_handle: Option<JoinHandle<Result<(), tonic::transport::Error>>>) -> Self {
        VLSKeysManager {
            async_runtime,
            keys_manager,
            _server_handle: server_handle
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
        Self { runtime: self.runtime.clone() }
    }
}
