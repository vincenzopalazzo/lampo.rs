use keys_manager::LampoKeysManager;
use lampo_common::vls::proxy::portfront::SignerPortFront;
use lampo_common::vls::proxy::vls_frontend::{frontend::DummySourceFactory, Frontend};
use lampo_common::vls::proxy::vls_protocol_client::{DynKeysInterface, SpendableKeysInterface};
use lampo_common::vls::proxy::vls_protocol_client::{DynSigner, KeysManagerClient};
use lampo_common::vls::signer::bitcoin::{Address, Network};
use lampo_common::vls::triggered::Listener;
use lampo_common::vls::url::Url;
use protocol_handler::LampoVLSInProcess;
use signer::LampoVLSSignerPort;
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
) -> Box<dyn SpendableKeysInterface<Signer = DynSigner>> {
    let protocol_handler = Arc::new(LampoVLSInProcess::new(sweep_address.clone(), network));
    let signer_port = Arc::new(LampoVLSSignerPort::new(protocol_handler.clone()));
    let source_factory = Arc::new(DummySourceFactory::new(lampo_data_dir, network));
    // The SignerPortFront provide a client RPC interface to the core MultiSigner and Node objects via a communications link.
    let signer_port_front = Arc::new(SignerPortFront::new(signer_port, network));
    let frontend = Frontend::new(
        signer_port_front,
        source_factory,
        bitcoin_rpc_url,
        shutdown_signal,
    );
    frontend.start();
    let client = KeysManagerClient::new(protocol_handler, network.to_string());
    let keys_manager = LampoKeysManager::new(client, sweep_address);
    Box::new(keys_manager)
}

pub enum SignerType {
    InProcess,
    GrpcRemote,
}

pub struct SignerParams {
    network: Network,
    lampo_data_dir: String,
    sweep_address: Address,
    bitcoin_rpc_url: Url,
    shutdown_signal: Listener,
}

impl SignerParams {
    pub fn new(
        network: Network,
        lampo_data_dir: String,
        sweep_address: Address,
        bitcoin_rpc_url: Url,
        shutdown_signal: Listener,
    ) -> Self {
        SignerParams {
            network,
            lampo_data_dir,
            shutdown_signal,
            sweep_address,
            bitcoin_rpc_url,
        }
    }
}

impl SignerType {
    fn make_signer(
        &self,
        params: SignerParams,
    ) -> Box<dyn SpendableKeysInterface<Signer = DynSigner>> {
        match self {
            SignerType::InProcess => make_in_process_signer(
                params.network,
                params.lampo_data_dir,
                params.sweep_address,
                params.bitcoin_rpc_url,
                params.shutdown_signal,
            ),
            SignerType::GrpcRemote => unimplemented!(),
        }
    }
}

fn get_keys_manager(
    params: SignerParams,
    signer_type: SignerType,
) -> Box<dyn SpendableKeysInterface<Signer = DynSigner>> {
    signer_type.make_signer(params)
}

pub struct LampoKeys {
    pub keys_manager: Arc<DynKeysInterface>,
}

impl LampoKeys {
    pub fn new(signer_type: SignerType, params: SignerParams) -> Self {
        let keys_manager = get_keys_manager(params, signer_type);
        LampoKeys {
            keys_manager: Arc::new(DynKeysInterface::new(keys_manager)),
        }
    }
}
