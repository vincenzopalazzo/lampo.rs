use std::sync::Arc;
use std::str::FromStr;
use crate::conf::LampoConf;
use crate::keys::KeysManagerFactory;
use bitcoin::Network;
use triggered::{trigger, Listener};
use url::Url;
use vls_proxy::portfront::SignerPortFront;
use vls_proxy::vls_frontend::frontend::SourceFactory;
use vls_proxy::vls_frontend::Frontend;
use vls_proxy::vls_protocol_client::KeysManagerClient;
use vls_proxy::vls_protocol_client::{Error, Transport, SignerPort};
use vls_proxy::vls_protocol_signer::handler::Handler;
use vls_proxy::vls_protocol_signer::handler::{HandlerBuilder, RootHandler};
use vls_proxy::vls_protocol_signer::vls_protocol::{model::PubKey, msgs};
use lightning_signer::node::NodeServices;
use lightning_signer::persist::DummyPersister;
use lightning_signer::policy::simple_validator::make_simple_policy;
use lightning_signer::policy::simple_validator::SimpleValidatorFactory;
use lightning_signer::signer::ClockStartingTimeFactory;
use lightning_signer::util::clock::StandardClock;

use async_trait::async_trait;

pub struct VLSKeysManagerFactory;

impl KeysManagerFactory for VLSKeysManagerFactory {
    type GenericKeysManager = KeysManagerClient;

    fn create_keys_manager(&self, conf: &LampoConf, seed: &[u8; 32]) -> Self::GenericKeysManager {
        let config = SignerConfig::new(conf, *seed);
        SignerType::InProcess.create_keys_manager(config)
    }
}

struct SignerConfig {
    network: Network,
    lampo_data_dir: String,
    bitcoin_rpc_url: Url,
    shutdown_signal: Listener,
    seed: [u8; 32],
}

impl SignerConfig {
    pub fn new(conf: &LampoConf, seed: [u8; 32]) -> Self {
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
                let protocol_handler = Arc::new(InProcessProtocolHandler::new(config.network, &config.seed));

                let signer_port = Arc::new(VLSSignerPort::new(protocol_handler.clone()));
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

/// The `LampoVLSInProcess` represents a VLS client with a Null Transport.
/// Designed to run VLS in-process, but still performs the VLS protocol, No Persistence.
pub struct InProcessProtocolHandler {
    pub handler: RootHandler
}

/// Describe method to handle messages using the VLS protocol for Singer and Channel API.
impl Transport for InProcessProtocolHandler {
    /// Perform a call for the Signer Protocol API
    fn node_call(&self, msg: Vec<u8>) -> Result<Vec<u8>, Error> {
        // Deserialize the incoming message
        let message = msgs::from_vec(msg)?;
        // Handle the message using RootHandler
        let (result, _) = self.handler.handle(message).map_err(|_| Error::Transport)?;
        Ok(result.as_vec())
    }

    // Perform a call for the Channel Protocol API
    fn call(&self, db_id: u64, peer_id: PubKey, msg: Vec<u8>) -> Result<Vec<u8>, Error> {
        // Deserialize the incoming message
        let message = msgs::from_vec(msg)?;
        // Creating a ChannelHandler
        let handler = self.handler.for_new_client(0, peer_id, db_id);
        // Handle the message using ChannelHandler
        let (result, _) = handler.handle(message).map_err(|_| Error::Transport)?;
        Ok(result.as_vec())
    }
}

impl InProcessProtocolHandler {
    // Initialize the ProtocolHandler with Default Configuration, No Persistence
    pub fn new(network: Network, seed: &[u8; 32]) -> Self {
        // Create a dummy persister (no persistence)
        let persister = Arc::new(DummyPersister);
        // Define an allowlist with the given address
        let allowlist = vec![];
        // Create a simple policy for the network
        let policy = make_simple_policy(network);
        // Create Validators using SimpleValidatorFactory with the policy
        let validator_factory = Arc::new(SimpleValidatorFactory::new_with_policy(policy));
        let starting_time_factory = ClockStartingTimeFactory::new();
        let clock = Arc::new(StandardClock());
        let services = NodeServices {
            validator_factory,
            starting_time_factory,
            persister,
            clock,
        };
        let (root_handler_builder, _) = HandlerBuilder::new(network, 0, services, seed.to_owned())
            .allowlist(allowlist)
            .build()
            .expect("Cannot Build The Root Handler");
        let root_handler = root_handler_builder.into_root_handler();
        InProcessProtocolHandler {
            handler: root_handler,
        }
    }
}


pub struct VLSSignerPort {
    protocol_handler: Arc<dyn Transport>,
}

impl VLSSignerPort {
    pub fn new(protocol_handler: Arc<dyn Transport>) -> Self {
        VLSSignerPort { protocol_handler }
    }
}

#[async_trait]
impl SignerPort for VLSSignerPort {
    async fn handle_message(&self, message: Vec<u8>) -> Result<Vec<u8>, Error> {
        self.protocol_handler.node_call(message)
    }
    fn is_ready(&self) -> bool {
        true
    }
}
