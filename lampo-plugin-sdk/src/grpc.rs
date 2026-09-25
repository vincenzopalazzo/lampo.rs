//! Plugin side of the local gRPC transport.
//!
//! The daemon spawns this process with `--lampo-socket`. Each `HandleRpc`
//! runs on its own task, so `foo` can call `yoooo` on a second connection
//! while the first handler is still awaiting.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use lampo_plugin::transport::grpc::proto::lampo_plugin_server::{LampoPlugin, LampoPluginServer};
use lampo_plugin::transport::grpc::proto::{
    HookRequest, HookResponse as ProtoHookResponse, InitRequest, InitResponse, ManifestRequest,
    ManifestResponse, NotifyRequest, NotifyResponse, RpcRequest, RpcResponse, ShutdownRequest,
    ShutdownResponse,
};
use lampo_plugin::transport::uds::UnixIncoming;
use serde_json::Value;
use tokio::sync::{oneshot, RwLock};
use tonic::{Request, Response, Status};

use crate::{HookHandler, InitHandler, NotifyHandler, PluginManifest, RpcHandler};

pub struct PluginService {
    manifest: PluginManifest,
    rpc_handlers: HashMap<String, RpcHandler>,
    hook_handlers: HashMap<String, HookHandler>,
    notify_handlers: HashMap<String, NotifyHandler>,
    on_init: Option<InitHandler>,
    init: Arc<RwLock<Option<Value>>>,
    shutdown: tokio::sync::Mutex<Option<oneshot::Sender<()>>>,
}

impl PluginService {
    pub fn new(
        manifest: PluginManifest,
        rpc_handlers: HashMap<String, RpcHandler>,
        hook_handlers: HashMap<String, HookHandler>,
        notify_handlers: HashMap<String, NotifyHandler>,
        on_init: Option<InitHandler>,
        init: Arc<RwLock<Option<Value>>>,
        shutdown: oneshot::Sender<()>,
    ) -> Self {
        Self {
            manifest,
            rpc_handlers,
            hook_handlers,
            notify_handlers,
            on_init,
            init,
            shutdown: tokio::sync::Mutex::new(Some(shutdown)),
        }
    }
}

#[tonic::async_trait]
impl LampoPlugin for PluginService {
    async fn get_manifest(
        &self,
        _request: Request<ManifestRequest>,
    ) -> Result<Response<ManifestResponse>, Status> {
        let manifest_json = serde_json::to_string(&self.manifest)
            .map_err(|err| Status::internal(err.to_string()))?;
        Ok(Response::new(ManifestResponse { manifest_json }))
    }

    async fn init(&self, request: Request<InitRequest>) -> Result<Response<InitResponse>, Status> {
        let params: Value = serde_json::from_str(&request.into_inner().config_json)
            .unwrap_or(Value::Object(Default::default()));
        *self.init.write().await = Some(params.clone());
        if let Some(on_init) = &self.on_init {
            if let Err(err) = on_init(params).await {
                return Ok(Response::new(InitResponse {
                    disable_message: err,
                }));
            }
        }
        Ok(Response::new(InitResponse {
            disable_message: String::new(),
        }))
    }

    async fn handle_rpc(
        &self,
        request: Request<RpcRequest>,
    ) -> Result<Response<RpcResponse>, Status> {
        let request = request.into_inner();
        let params: Value =
            serde_json::from_str(&request.params_json).unwrap_or(Value::Object(Default::default()));
        let Some(handler) = self.rpc_handlers.get(&request.method) else {
            return Ok(Response::new(RpcResponse {
                result_json: String::new(),
                error_message: format!("method not found: {}", request.method),
                error_code: -32601,
            }));
        };
        match handler(params).await {
            Ok(result) => {
                let result_json = serde_json::to_string(&result)
                    .map_err(|err| Status::internal(err.to_string()))?;
                Ok(Response::new(RpcResponse {
                    result_json,
                    error_message: String::new(),
                    error_code: 0,
                }))
            }
            Err(message) => Ok(Response::new(RpcResponse {
                result_json: String::new(),
                error_message: message,
                error_code: -32000,
            })),
        }
    }

    async fn handle_hook(
        &self,
        request: Request<HookRequest>,
    ) -> Result<Response<ProtoHookResponse>, Status> {
        let request = request.into_inner();
        let params: Value = serde_json::from_str(&request.payload_json)
            .unwrap_or(Value::Object(Default::default()));
        let hook_name = request
            .hook_name
            .strip_prefix("hook/")
            .unwrap_or(&request.hook_name);
        let response = if let Some(handler) = self.hook_handlers.get(hook_name) {
            handler(params).await
        } else {
            crate::HookResponse::Continue { payload: None }
        };
        let response_json =
            serde_json::to_string(&response).map_err(|err| Status::internal(err.to_string()))?;
        Ok(Response::new(ProtoHookResponse { response_json }))
    }

    async fn notify(
        &self,
        request: Request<NotifyRequest>,
    ) -> Result<Response<NotifyResponse>, Status> {
        let request = request.into_inner();
        let params: Value = serde_json::from_str(&request.payload_json)
            .unwrap_or(Value::Object(Default::default()));
        if let Some(handler) = self.notify_handlers.get(&request.topic) {
            handler(params).await;
        }
        Ok(Response::new(NotifyResponse {}))
    }

    async fn shutdown(
        &self,
        _request: Request<ShutdownRequest>,
    ) -> Result<Response<ShutdownResponse>, Status> {
        if let Some(tx) = self.shutdown.lock().await.take() {
            let _ = tx.send(());
        }
        Ok(Response::new(ShutdownResponse {}))
    }
}

/// Listen on `addr` (`127.0.0.1:0` picks a port) until `Shutdown`.
/// Prints `lampo-listen <addr>` so the daemon can dial it.
pub async fn serve(
    addr: std::net::SocketAddr,
    manifest: PluginManifest,
    rpc_handlers: HashMap<String, RpcHandler>,
    hook_handlers: HashMap<String, HookHandler>,
    notify_handlers: HashMap<String, NotifyHandler>,
    on_init: Option<InitHandler>,
    init: Arc<RwLock<Option<Value>>>,
) {
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            log::error!(target: "plugin-sdk", "bind `{addr}`: {err}");
            return;
        }
    };
    let local = match listener.local_addr() {
        Ok(local) => local,
        Err(err) => {
            log::error!(target: "plugin-sdk", "local_addr: {err}");
            return;
        }
    };
    // The daemon reads this line before any other stdout. Do not log first.
    println!("lampo-listen {local}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    let (tx, rx) = oneshot::channel();
    let service = PluginService::new(
        manifest,
        rpc_handlers,
        hook_handlers,
        notify_handlers,
        on_init,
        init,
        tx,
    );
    log::info!(target: "plugin-sdk", "plugin listening on {local}");
    let server = tonic::transport::Server::builder()
        .add_service(LampoPluginServer::new(service))
        .serve_with_incoming(incoming);
    tokio::pin!(server);
    tokio::select! {
        result = &mut server => {
            if let Err(err) = result {
                log::error!(target: "plugin-sdk", "plugin server stopped: {err}");
            }
        }
        _ = rx => {
            log::info!(target: "plugin-sdk", "plugin shutdown");
        }
    }
}

/// `--lampo-listen 127.0.0.1:0` from the daemon. `None` means stdio.
pub fn listen_from_args() -> Option<std::net::SocketAddr> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            eprintln!("Usage: plugin --lampo-listen 127.0.0.1:0");
            eprintln!("  --lampo-listen <addr>   loopback gRPC address");
            std::process::exit(0);
        }
        if arg == "--lampo-listen" {
            return args.next().and_then(|value| value.parse().ok());
        }
        if let Some(value) = arg.strip_prefix("--lampo-listen=") {
            return value.parse().ok();
        }
    }
    None
}
