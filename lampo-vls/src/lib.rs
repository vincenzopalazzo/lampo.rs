pub mod protocol_handler;
pub mod signer;

use std::str::FromStr;
use std::sync::Arc;

use vls_proxy::portfront::SignerPortFront;
use vls_proxy::vls_frontend::frontend::SourceFactory;
use vls_proxy::vls_frontend::Frontend;
use vls_proxy::vls_protocol_client::{KeysManagerClient, SignerClient};
use triggered::{trigger, Listener};
use url::Url;

use lampo_common::bitcoin::Network;
use lampo_common::conf::LampoConf;
// use lampo_common::keys::KeysManagerFactory;


// pub struct VLSKeysManagerFactory;

// impl KeysManagerFactory for VLSKeysManagerFactory {
//     type GenericKeysManager = KeysManagerClient;

//     fn create_keys_manager(&self, conf: Arc<LampoConf>, seed: &[u8; 32]) -> Self::GenericKeysManager {
//         let config = SignerConfig::new(conf, *seed);
//         SignerType::InProcess.create_keys_manager(config)
//     }
// }

struct SignerConfig {
    network: Network,
    lampo_data_dir: String,
    bitcoin_rpc_url: Url,
    shutdown_signal: Listener,
    seed: [u8; 32],
}

impl SignerConfig {
    pub fn new(conf: Arc<LampoConf>, seed: [u8; 32]) -> Self {
        let (_, listener) = trigger();
        SignerConfig {
            network: conf.network,
            lampo_data_dir: conf.root_path.clone(),
            bitcoin_rpc_url: Url::from_str(conf.core_url.as_ref().unwrap()).unwrap(),
            shutdown_signal: listener,
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
    fn create_keys_manager(&self, config: SignerConfig) -> KeysManagerClient {
        match self {
            SignerType::InProcess => {
                // This will create a handler that will manage the VLS protocol operations
                let protocol_handler = Arc::new(protocol_handler::InProcessProtocolHandler::new(config.network, &config.seed));
                let signer_port = Arc::new(signer::VLSSignerPort::new(protocol_handler.clone()));
                // This factory manages data sources but doesn't actually do anything (dummy).
                let source_factory = Arc::new(SourceFactory::new(config.lampo_data_dir, config.network));
                // The SignerPortFront provide a client RPC interface to the core MultiSigner and Node objects via a communications link.
                let signer_port_front = Arc::new(SignerPortFront::new(signer_port, config.network));
                // The frontend acts like a proxy to handle communication between the Signer and the Node
                let frontend = Frontend::new(
                    signer_port_front,
                    source_factory,
                    config.bitcoin_rpc_url,
                    config.shutdown_signal,
                );
                // Starts the frontend (probably discuss is this the right place to start or not)
                frontend.start();
                // Similar to KeysManager from LDK but here as all Key related operations are happening on the
                // signer, thus we need a client to facilitate that
                KeysManagerClient::new(protocol_handler, config.network.to_string())
            }
            SignerType::GrpcRemote => unimplemented!()
        }
    }
}
