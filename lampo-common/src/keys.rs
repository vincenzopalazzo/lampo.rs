use std::{sync::Arc, time::SystemTime};

use bitcoin::secp256k1::SecretKey;
use tokio::runtime::Runtime;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use tonic::transport::Error as TonicError;
use triggered::{Listener, Trigger};
use lightning::sign::{NodeSigner, OutputSpender, SignerProvider};
use vls_proxy::vls_protocol_client::{KeysManagerClient, SignerClient};

use crate::{conf::LampoConf, ldk::sign::{EntropySource, KeysManager}};

/// Lampo keys implementations
pub struct LampoKeys {
    pub keys_manager: Arc<LampoKeysManager>,
}

pub trait KeysManagerFactory {
    type GenericKeysManager: SignerProvider;

    fn create_keys_manager(&self, conf: Arc<LampoConf>, seed: &[u8; 32], vls_port: Option<u16>, shutter: Option<Shutter>) -> GrpcKeysManager;
} 

// pub struct LDKKeysManagerFactory;

// impl KeysManagerFactory for LDKKeysManagerFactory {
//     type GenericKeysManager = KeysManager;

//     fn create_keys_manager(&self, _ : Arc<LampoConf>, seed: &[u8; 32], _: Option<u16>, _: Option<Shutter>, ) -> Self::GenericKeysManager {
//         let start_time = SystemTime::now()
//             .duration_since(SystemTime::UNIX_EPOCH)
//             .unwrap();
//         KeysManager::new(seed, start_time.as_secs(), start_time.subsec_nanos())
//     }
// }

impl LampoKeys {
    pub fn new(_seed: [u8; 32], _conf: Arc<LampoConf>, keys_manager: KeysManagerClient) -> Self {
        LampoKeys {
            keys_manager: Arc::new(LampoKeysManager::new(keys_manager)),
        }
    }

    #[cfg(debug_assertions)]
    pub fn with_channel_keys(_seed: [u8; 32], channels_keys: String, _conf: Arc<LampoConf>, keys_manager: KeysManagerClient) -> Self {

        let keys = channels_keys.split('/').collect::<Vec<_>>();

        let mut manager =
            LampoKeysManager::new(keys_manager);
        manager.set_channels_keys(
            keys[1].to_string(),
            keys[2].to_string(),
            keys[3].to_string(),
            keys[4].to_string(),
            keys[5].to_string(),
            keys[6].to_string(),
        );
        LampoKeys {
            keys_manager: Arc::new(manager),
        }
    }

    pub fn inner(&self) -> Arc<LampoKeysManager> {
        self.keys_manager.clone()
    }
}

pub struct LampoKeysManager {
    pub(crate) inner: KeysManagerClient,

    funding_key: Option<SecretKey>,
    revocation_base_secret: Option<SecretKey>,
    payment_base_secret: Option<SecretKey>,
    delayed_payment_base_secret: Option<SecretKey>,
    htlc_base_secret: Option<SecretKey>,
    shachain_seed: Option<[u8; 32]>,
}

impl LampoKeysManager {
    pub fn new(keys_manager: KeysManagerClient) -> Self {
        Self {
            inner: keys_manager,
            funding_key: None,
            revocation_base_secret: None,
            payment_base_secret: None,
            delayed_payment_base_secret: None,
            htlc_base_secret: None,
            shachain_seed: None,
        }
    }

    // FIXME: put this under a debug a feature flag like `unsafe_channel_keys`
    #[cfg(debug_assertions)]
    pub fn set_channels_keys(
        &mut self,
        funding_key: String,
        revocation_base_secret: String,
        payment_base_secret: String,
        delayed_payment_base_secret: String,
        htlc_base_secret: String,
        _shachain_seed: String,
    ) {
        use std::str::FromStr;

        self.funding_key = Some(SecretKey::from_str(&funding_key).unwrap());
        self.revocation_base_secret = Some(SecretKey::from_str(&revocation_base_secret).unwrap());
        self.payment_base_secret = Some(SecretKey::from_str(&payment_base_secret).unwrap());
        self.delayed_payment_base_secret =
            Some(SecretKey::from_str(&delayed_payment_base_secret).unwrap());
        self.htlc_base_secret = Some(SecretKey::from_str(&htlc_base_secret).unwrap());
        self.shachain_seed = Some(self.inner.get_secure_random_bytes())
    }
}

impl EntropySource for LampoKeysManager {
    fn get_secure_random_bytes(&self) -> [u8; 32] {
        self.inner.get_secure_random_bytes()
    }
}

impl NodeSigner for LampoKeysManager {
    fn ecdh(
        &self,
        recipient: lightning::sign::Recipient,
        other_key: &bitcoin::secp256k1::PublicKey,
        tweak: Option<&bitcoin::secp256k1::Scalar>,
    ) -> Result<bitcoin::secp256k1::ecdh::SharedSecret, ()> {
        self.inner.ecdh(recipient, other_key, tweak)
    }

    fn get_inbound_payment_key_material(&self) -> lightning::sign::KeyMaterial {
        self.inner.get_inbound_payment_key_material()
    }

    fn get_node_id(
        &self,
        recipient: lightning::sign::Recipient,
    ) -> Result<bitcoin::secp256k1::PublicKey, ()> {
        self.inner.get_node_id(recipient)
    }

    fn sign_bolt12_invoice(
        &self,
        invoice: &lightning::offers::invoice::UnsignedBolt12Invoice,
    ) -> Result<bitcoin::secp256k1::schnorr::Signature, ()> {
        self.inner.sign_bolt12_invoice(invoice)
    }

    fn sign_bolt12_invoice_request(
        &self,
        invoice_request: &lightning::offers::invoice_request::UnsignedInvoiceRequest,
    ) -> Result<bitcoin::secp256k1::schnorr::Signature, ()> {
        self.inner.sign_bolt12_invoice_request(invoice_request)
    }

    fn sign_gossip_message(
        &self,
        msg: lightning::ln::msgs::UnsignedGossipMessage,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.inner.sign_gossip_message(msg)
    }

    fn sign_invoice(
        &self,
        hrp_bytes: &[u8],
        invoice_data: &[bitcoin::bech32::u5],
        recipient: lightning::sign::Recipient,
    ) -> Result<bitcoin::secp256k1::ecdsa::RecoverableSignature, ()> {
        self.inner.sign_invoice(hrp_bytes, invoice_data, recipient)
    }
}

impl OutputSpender for LampoKeysManager {
    fn spend_spendable_outputs<C: bitcoin::secp256k1::Signing>(
        &self,
        descriptors: &[&lightning::sign::SpendableOutputDescriptor],
        outputs: Vec<bitcoin::TxOut>,
        change_destination_script: bitcoin::ScriptBuf,
        feerate_sat_per_1000_weight: u32,
        locktime: Option<bitcoin::absolute::LockTime>,
        secp_ctx: &bitcoin::secp256k1::Secp256k1<C>,
    ) -> Result<bitcoin::Transaction, ()> {
        self.inner.spend_spendable_outputs(
            descriptors,
            outputs,
            change_destination_script,
            feerate_sat_per_1000_weight,
            locktime,
            secp_ctx,
        )
    }
}

impl SignerProvider for LampoKeysManager {
    // FIXME: this should be the same of the inner
    type EcdsaSigner = SignerClient;

    fn derive_channel_signer(
        &self,
        channel_value_satoshis: u64,
        channel_keys_id: [u8; 32],
    ) -> Self::EcdsaSigner {
        // if self.funding_key.is_some() {
        //     // FIXME(vincenzopalazzo): make this a general
        //     let commitment_seed = [
        //         255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
        //         255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
        //     ];
        //     return InMemorySigner::new(
        //         &Secp256k1::new(),
        //         self.funding_key.unwrap(),
        //         self.revocation_base_secret.unwrap(),
        //         self.payment_base_secret.unwrap(),
        //         self.delayed_payment_base_secret.unwrap(),
        //         self.htlc_base_secret.unwrap(),
        //         commitment_seed,
        //         channel_value_satoshis,
        //         channel_keys_id,
        //         self.shachain_seed.unwrap(),
        //     );
        // }
        self.inner
            .derive_channel_signer(channel_value_satoshis, channel_keys_id)
    }

    fn generate_channel_keys_id(
        &self,
        inbound: bool,
        channel_value_satoshis: u64,
        user_channel_id: u128,
    ) -> [u8; 32] {
        self.inner
            .generate_channel_keys_id(inbound, channel_value_satoshis, user_channel_id)
    }

    fn get_destination_script(&self, channel_keys_id: [u8; 32]) -> Result<bitcoin::ScriptBuf, ()> {
        self.inner.get_destination_script(channel_keys_id)
    }

    fn get_shutdown_scriptpubkey(&self) -> Result<lightning::ln::script::ShutdownScript, ()> {
        self.inner.get_shutdown_scriptpubkey()
    }

    fn read_chan_signer(
        &self,
        reader: &[u8],
    ) -> Result<Self::EcdsaSigner, lightning::ln::msgs::DecodeError> {
        self.inner.read_chan_signer(reader)
    }
}



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


pub struct GrpcKeysManager {
    pub async_runtime: Arc<AsyncRuntime>,
    pub keys_manager: KeysManagerClient,
    pub _server_handle: JoinHandle<Result<(), TonicError>>
}

impl GrpcKeysManager {
    pub fn new(async_runtime: Arc<AsyncRuntime>, keys_manager: KeysManagerClient, server_handle: JoinHandle<Result<(), tonic::transport::Error>>) -> Self {
        GrpcKeysManager {
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

    // pub fn get_runtime(&self) -> Runtime {
    //     self.runtime
    // }

    // pub fn get_keys_manager(&self) -> KeysManagerClient {
    //     self.keys_manager
    // }
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
