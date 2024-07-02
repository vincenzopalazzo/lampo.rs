use keys_manager::LampoKeysManager;
use std::str::FromStr;
use lampo_common::conf::LampoConf;
use lampo_common::ldk::sign::SignerProvider;
use vls_proxy::portfront::SignerPortFront;
use vls_proxy::vls_frontend::frontend::SourceFactory;
use vls_proxy::vls_frontend::Frontend;
use vls_proxy::vls_protocol_client::DynKeysInterface;
use vls_proxy::vls_protocol_client::DynSigner;
use vls_proxy::vls_protocol_client::KeysManagerClient;
use lampo_common::bitcoin::{Address, Network};
use triggered::{trigger, Listener};
use url::Url;
use protocol_handler::LampoVLSInProcess;
use signer::LampoVLSSignerPort;
use vls_proxy::vls_protocol_client::SpendableKeysInterface;
use std::sync::Arc;

mod keys_manager;
mod protocol_handler;
mod signer;
mod util;

fn make_in_process_signer(
    network: Network,
    lampo_data_dir: String,
    sweep_address: Address,
    bitcoin_rpc_url: Url,
    shutdown_signal: Listener,
    seed: [u8; 32],
) -> Box<dyn SpendableKeysInterface<EcdsaSigner = DynSigner>> {
    // This will create a handler that will manage the VLS protocol operations
    let protocol_handler = Arc::new(LampoVLSInProcess::new(sweep_address.clone(), network, seed));
    let signer_port = Arc::new(LampoVLSSignerPort::new(protocol_handler.clone()));
    // This factory manages data sources but doesn't actually do anything (dummy).
    let source_factory = Arc::new(SourceFactory::new(lampo_data_dir, network));
    // The SignerPortFront provide a client RPC interface to the core MultiSigner and Node objects via a communications link.
    let signer_port_front = Arc::new(SignerPortFront::new(signer_port, network));
    // The frontend acts like a proxy to handle communication between the Signer and the Node
    let frontend = Frontend::new(
        signer_port_front,
        source_factory,
        bitcoin_rpc_url,
        shutdown_signal,
    );
    // Starts the frontend (probably discuss is this the right place to start or not)
    frontend.start();
    // Similar to KeysManager from LDK but here as all Key related operations are happening on the
    // signer, thus we need a client to facilitate that
    let client = KeysManagerClient::new(protocol_handler, network.to_string());
    // Create a LampoKeysManager object
    let keys_manager = LampoKeysManager::new(client, sweep_address);
    Box::new(keys_manager)
}

/// Enum specifying types of signers
pub enum SignerType {
    InProcess,  // InProcess Signer
    GrpcRemote, // Remote Signer (not implemented yet)
}

/// Struct holding parameters needed to create a signer
struct SignerParams {
    network: Network,
    lampo_data_dir: String,
    sweep_address: Address,
    bitcoin_rpc_url: Url,
    shutdown_signal: Listener,
    seed: [u8; 32],
}

impl SignerParams {
    pub fn new(
        network: Network,
        lampo_data_dir: String,
        sweep_address: Address,
        bitcoin_rpc_url: Url,
        shutdown_signal: Listener,
        seed: [u8; 32],
    ) -> Self {
        SignerParams {
            network,
            lampo_data_dir,
            shutdown_signal,
            sweep_address,
            bitcoin_rpc_url,
            seed,
        }
    }
}

impl SignerType {
    /// Method to create a signer based on the signer type
    fn make_signer(
        &self,
        params: SignerParams,
    ) -> Box<dyn SpendableKeysInterface<EcdsaSigner = DynSigner>> {
        match self {
            // Create an InProcess Signer
            SignerType::InProcess => make_in_process_signer(
                params.network,
                params.lampo_data_dir,
                params.sweep_address,
                params.bitcoin_rpc_url,
                params.shutdown_signal,
                params.seed,
            ),
            // Remote signer is not implemented
            SignerType::GrpcRemote => unimplemented!(),
        }
    }
}

/// Returns a keys manager based on the provided signer type
fn get_keys_manager(
    conf: &LampoConf,
    seed: [u8; 32],
    signer_type: SignerType,
    address: Address,
) -> Box<dyn SpendableKeysInterface<EcdsaSigner = DynSigner>> {
    let (_, listener) = trigger();
    let params = SignerParams::new(conf.network.clone(), conf.root_path, address, Url::from_str(conf.core_url).unwrap(), listener, seed);
    signer_type.make_signer(params)
}

/// Struct encapsulating a keys manager
pub struct LampoKeys {
    pub keys_manager: Arc<dyn SignerProvider<EcdsaSigner = DynSigner>>,
}

impl LampoKeys {
    pub fn new(signer_type: SignerType, conf: &LampoConf, seed: [u8; 32], sweep_address: Address) -> Self {
        let keys_manager = get_keys_manager(conf, seed, signer_type, sweep_address);
        LampoKeys {
            keys_manager: Arc::new(DynKeysInterface::new(keys_manager)),
        }
    }
}
