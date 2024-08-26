pub mod protocol_handler;
pub mod signer;
pub mod frontend_starter;

use std::net::{Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use protocol_handler::GrpcProtocolHandler;
use signer::VLSSignerPort;
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
use lampo_common::conf::LampoConf;
use lampo_common::keys::{GrpcKeysManager, AsyncRuntime, KeysManagerFactory, Shutter};

pub struct VLSKeysManagerFactory;

impl KeysManagerFactory for VLSKeysManagerFactory {
    type GenericKeysManager = KeysManagerClient;

    fn create_keys_manager(&self, conf: Arc<LampoConf>, seed: &[u8; 32], vls_port: Option<u16>, shutter: Option<Shutter>) -> GrpcKeysManager {
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
    shutter: Option<Shutter>,
}

impl SignerConfig {
    pub fn new(conf: Arc<LampoConf>, seed: [u8; 32], vls_port: Option<u16>, shutter: Option<Shutter>) -> Self {
        let (_, listener) = trigger();
        SignerConfig {
            network: conf.network,
            lampo_data_dir: conf.root_path.clone(),
            bitcoin_rpc_url: Url::from_str(conf.core_url.as_ref().unwrap()).unwrap(),
            shutdown_signal: listener,
            vls_port,
            shutter,
            seed,
        }
    }
}
pub enum SignerType {
    InProcess,  // InProcess Signer
    GrpcRemote, // Remote Signer (not implemented yet)
}

impl SignerType {
    /// Method to create a signer based on the signer type
    fn create_keys_manager(&self, config: SignerConfig) -> GrpcKeysManager {
        match self {
            SignerType::InProcess => unimplemented!(),
            // SignerType::InProcess => {
            //     // This will create a handler that will manage the VLS protocol operations
            //     let protocol_handler = Arc::new(protocol_handler::InProcessProtocolHandler::new(config.network, &config.seed));

            //     let signer_port = Arc::new(signer::VLSSignerPort::new(protocol_handler.clone()));
            //     // This factory manages data sources but doesn't actually do anything (dummy).
            //     let source_factory = Arc::new(SourceFactory::new(config.lampo_data_dir, config.network));
            //     // The SignerPortFront provide a client RPC interface to the core MultiSigner and Node objects via a communications link.
            //     let signer_port_front = Arc::new(SignerPortFront::new(signer_port, config.network));
            //     // The frontend acts like a proxy to handle communication between the Signer and the Node
            //     let frontend = Frontend::new(
            //         signer_port_front,
            //         source_factory,
            //         config.bitcoin_rpc_url,
            //         config.shutdown_signal,
            //     );

            //     FrontendStarter::start_frontend(Arc::new(frontend));
            //     // Starts the frontend (probably discuss is this the right place to start or not)
            //     // frontend.start();
            //     // Similar to KeysManager from LDK but here as all Key related operations are happening on the
            //     // signer, thus we need a client to facilitate that
            //     KeysManagerClient::new(protocol_handler, config.network.to_string())
            // }
            SignerType::GrpcRemote => {
    
                let async_runtime = Arc::new(AsyncRuntime::new());

                let (keys_manager, server_handle) = async_runtime.block_on(async {
                    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, config.vls_port.unwrap()));
                    let incoming = TcpIncoming::new(addr, false, None).expect("listen incoming");
                    let init_message_cache = Arc::new(Mutex::new(InitMessageCache::new()));
                    let shutter = config.shutter.unwrap();
                    let server = HsmdService::new(shutter.trigger.clone(), shutter.signal.clone(), init_message_cache);
                    let sender = server.sender();

                    let signer_handle = async_runtime.handle().clone();

                    let hello = signer_handle.spawn(server.start(incoming, shutter.signal.clone()));

                    // let transport = Arc::new(
                    //     signer_handle
                    //         .spawn(GrpcProtocolHandler::new(sender))
                    //         .await
                    //         .expect("join")
                    //         .expect("gRPC transport init"),
                    // );
                    let transport = Arc::new(GrpcProtocolHandler::new(sender, async_runtime.clone()).await.expect("FUCKED!!!!"));

                    let source_factory = Arc::new(SourceFactory::new(config.lampo_data_dir, config.network));
                    let signer_port = Arc::new(VLSSignerPort::new(transport.clone()));
                    let frontend = Frontend::new(
                        Arc::new(SignerPortFront::new(signer_port, config.network)),
                        source_factory,
                        config.bitcoin_rpc_url,
                        shutter.signal,
                    );
                    frontend.start();

                    (KeysManagerClient::new(transport, config.network.to_string()), hello)
                });

                GrpcKeysManager::new(async_runtime, keys_manager, server_handle)
            }
        }
    }
}
