pub mod protocol_handler;
pub mod signer;
pub mod util;

use std::net::{Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use protocol_handler::GrpcProtocolHandler;
use signer::VLSSignerPort;
use util::{AsyncRuntime, VLSKeysManager};

use vls_proxy::grpc::adapter::HsmdService;
use vls_proxy::grpc::incoming::TcpIncoming;
use vls_proxy::grpc::signer_loop::InitMessageCache;
use vls_proxy::portfront::SignerPortFront;
use vls_proxy::vls_frontend::frontend::SourceFactory;
use vls_proxy::vls_frontend::Frontend;
use vls_proxy::vls_protocol_client::KeysManagerClient;
use triggered::{trigger, Listener};
use url::Url;

use lampo_common::bitcoin::Network;
use lampo_common::utils::shutter::Shutter;
use lampo_common::conf::LampoConf;

pub struct VLSKeys;

impl VLSKeys {
    pub fn create_keys_manager(&self, conf: Arc<LampoConf>, seed: &[u8; 32], vls_port: Option<u16>, shutter: Option<Arc<Shutter>>) -> VLSKeysManager {
        let config = SignerConfig::new(conf, *seed, vls_port, shutter);
        SignerType::GrpcRemote.create_keys_manager(config)
    }
}

struct SignerConfig {
    network: Network,
    lampo_data_dir: String,
    bitcoin_rpc_url: Url,
    shutdown_signal: Listener,
    seed: [u8; 32],
    vls_port: Option<u16>,
    shutter: Option<Arc<Shutter>>,
}

impl SignerConfig {
    pub fn new(conf: Arc<LampoConf>, seed: [u8; 32], vls_port: Option<u16>, shutter: Option<Arc<Shutter>>) -> Self {
        let (_, listener) = trigger();
        let url = Url::from_str(conf.core_url.as_ref().unwrap()).unwrap();

        let bitcoin_rpc_url = Url::from_str(
            format!(
                "http://{}:{}@{}:{}", 
                conf.core_user.as_ref().unwrap(), 
                conf.core_pass.as_ref().unwrap(), 
                url.host().unwrap(), 
                url.port().unwrap()
            ).as_ref()
        ).unwrap();

        SignerConfig {
            network: conf.network,
            lampo_data_dir: conf.root_path.clone(),
            shutdown_signal: listener,
            bitcoin_rpc_url,
            vls_port,
            shutter,
            seed,
        }
    }
}
pub enum SignerType {
    InProcess,  // InProcess Signer
    GrpcRemote, // Remote Signer
}

impl SignerType {
    /// Method to create a signer based on the signer type
    fn create_keys_manager(&self, config: SignerConfig) -> VLSKeysManager {
        match self {
            SignerType::InProcess => {
                let async_runtime = Arc::new(AsyncRuntime::new());

                let (keys_manager, server_handler) = async_runtime.block_on(async {
                    let protocol_handler = Arc::new(protocol_handler::InProcessProtocolHandler::new(config.network, &config.seed));

                    let signer_port = Arc::new(signer::VLSSignerPort::new(protocol_handler.clone()));

                    let source_factory = Arc::new(SourceFactory::new(config.lampo_data_dir, config.network));

                    let signer_port_front = Arc::new(SignerPortFront::new(signer_port, config.network));

                    let frontend = Frontend::new(
                        signer_port_front,
                        source_factory,
                        config.bitcoin_rpc_url,
                        config.shutdown_signal,
                    );

                    frontend.start();
                    
                    (KeysManagerClient::new(protocol_handler, config.network.to_string()), None)
                });

                VLSKeysManager::new(async_runtime, keys_manager, server_handler)

            },

            SignerType::GrpcRemote => {
                let async_runtime = Arc::new(AsyncRuntime::new());

                let (keys_manager, server_handle) = async_runtime.block_on(async {
                    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, config.vls_port.unwrap()));
                    let incoming = TcpIncoming::new(addr, false, None).expect("listen incoming");
                    let init_message_cache = Arc::new(Mutex::new(InitMessageCache::new()));
                    let shutter = config.shutter.unwrap();
                    let server = HsmdService::new(shutter.trigger(), shutter.signal(), init_message_cache);
                    let sender = server.sender();

                    let signer_handle = async_runtime.handle().clone();

                    let server_handler = signer_handle.spawn(server.start(incoming, shutter.signal()));

                    let transport = Arc::new(GrpcProtocolHandler::new(sender, async_runtime.clone()).await.expect("Cannot create gRPC transport"));

                    let source_factory = Arc::new(SourceFactory::new(config.lampo_data_dir, config.network));
                    let signer_port = Arc::new(VLSSignerPort::new(transport.clone()));
                    let frontend = Frontend::new(
                        Arc::new(SignerPortFront::new(signer_port, config.network)),
                        source_factory,
                        config.bitcoin_rpc_url,
                        shutter.signal()
                    );
                    frontend.start();

                    (KeysManagerClient::new(transport, config.network.to_string()), server_handler)
                });

                VLSKeysManager::new(async_runtime, keys_manager, Option::from(server_handle))
            }
        }
    }
}
