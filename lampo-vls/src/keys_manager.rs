use lampo_common::vls::proxy::vls_protocol_client::SpendableKeysInterface;
use lampo_common::vls::proxy::vls_protocol_client::{DynSigner, KeysManagerClient};
use lampo_common::vls::signer::bitcoin::secp256k1::ecdh::SharedSecret;
use lampo_common::vls::signer::bitcoin::secp256k1::ecdsa::RecoverableSignature;
use lampo_common::vls::signer::bitcoin::secp256k1::ecdsa::Signature as EcdsaSignature;
use lampo_common::vls::signer::bitcoin::secp256k1::schnorr::Signature;
use lampo_common::vls::signer::bitcoin::secp256k1::{All, Secp256k1};
use lampo_common::vls::signer::bitcoin::secp256k1::{PublicKey, Scalar};
use lampo_common::vls::signer::bitcoin::{bech32::u5, Address, Script};
use lampo_common::vls::signer::bitcoin::{Transaction, TxOut, Witness};
use lampo_common::vls::signer::invoice::bolt12::UnsignedBolt12Invoice;
use lampo_common::vls::signer::lightning::ln::{msgs, script::ShutdownScript};
use lampo_common::vls::signer::lightning::offers::invoice_request::UnsignedInvoiceRequest;
use lampo_common::vls::signer::lightning::sign::SpendableOutputDescriptor;
use lampo_common::vls::signer::lightning::sign::{EntropySource, NodeSigner, SignerProvider};
use lampo_common::vls::signer::lightning::sign::{KeyMaterial, Recipient};

use crate::util::create_spending_transaction;

#[allow(dead_code)]
/// Holds an instance of KeysManagerClient which interacts with the VLS protocol to fetch or manage keys.
// FIXME: This should be implemented under VLS?
pub struct LampoKeysManager {
    /// The KeysManagerClient is a client-side interface for interacting with the key management functionalities of the VLS signer.
    client: KeysManagerClient,
    sweep_address: Address,
}

impl LampoKeysManager {
    pub fn new(client: KeysManagerClient, sweep_address: Address) -> Self {
        LampoKeysManager {
            client,
            sweep_address,
        }
    }
}

// To get signer instances for individual channels.
impl SignerProvider for LampoKeysManager {
    type Signer = DynSigner;

    fn generate_channel_keys_id(
        &self,
        inbound: bool,
        channel_value_satoshis: u64,
        user_channel_id: u128,
    ) -> [u8; 32] {
        self.client
            .generate_channel_keys_id(inbound, channel_value_satoshis, user_channel_id)
    }

    fn derive_channel_signer(
        &self,
        channel_value_satoshis: u64,
        channel_keys_id: [u8; 32],
    ) -> Self::Signer {
        let client = self
            .client
            .derive_channel_signer(channel_value_satoshis, channel_keys_id);
        DynSigner::new(client)
    }

    fn read_chan_signer(&self, reader: &[u8]) -> Result<Self::Signer, msgs::DecodeError> {
        let signer = self.client.read_chan_signer(reader)?;
        Ok(DynSigner::new(signer))
    }

    fn get_destination_script(&self) -> Result<Script, ()> {
        self.client.get_destination_script()
    }
    fn get_shutdown_scriptpubkey(&self) -> Result<ShutdownScript, ()> {
        self.client.get_shutdown_scriptpubkey()
    }
}

// Source of entropy.
impl EntropySource for LampoKeysManager {
    fn get_secure_random_bytes(&self) -> [u8; 32] {
        self.client.get_secure_random_bytes()
    }
}

// Cryptographic operations at the scope level of a node
impl NodeSigner for LampoKeysManager {
    fn get_inbound_payment_key_material(&self) -> KeyMaterial {
        self.client.get_inbound_payment_key_material()
    }

    fn get_node_id(&self, recipient: Recipient) -> Result<PublicKey, ()> {
        self.client.get_node_id(recipient)
    }

    fn ecdh(
        &self,
        recipient: Recipient,
        other_key: &PublicKey,
        tweak: Option<&Scalar>,
    ) -> Result<SharedSecret, ()> {
        self.client.ecdh(recipient, other_key, tweak)
    }

    fn sign_invoice(
        &self,
        hrp_bytes: &[u8],
        invoice_data: &[u5],
        recipient: Recipient,
    ) -> Result<RecoverableSignature, ()> {
        self.client.sign_invoice(hrp_bytes, invoice_data, recipient)
    }
    fn sign_bolt12_invoice_request(
        &self,
        invoice_request: &UnsignedInvoiceRequest,
    ) -> Result<Signature, ()> {
        self.client.sign_bolt12_invoice_request(invoice_request)
    }

    fn sign_bolt12_invoice(&self, invoice: &UnsignedBolt12Invoice) -> Result<Signature, ()> {
        self.client.sign_bolt12_invoice(invoice)
    }
    fn sign_gossip_message(&self, msg: msgs::UnsignedGossipMessage) -> Result<EcdsaSignature, ()> {
        self.client.sign_gossip_message(msg)
    }
}

// Manages spending from descriptors that define how outputs are spent in transactions (Not Sure!!)
impl SpendableKeysInterface for LampoKeysManager {
    fn spend_spendable_outputs(
        &self,
        descriptors: &[&SpendableOutputDescriptor],
        outputs: Vec<TxOut>,
        change_destination_script: Script,
        feerate_sat_per_1000_weight: u32,
        _secp_ctx: &Secp256k1<All>,
    ) -> lampo_common::vls::anyhow::Result<Transaction> {
        let mut tx = create_spending_transaction(
            descriptors,
            outputs,
            Box::new(change_destination_script),
            feerate_sat_per_1000_weight,
        )?;
        let witnesses = self.client.sign_onchain_tx(&tx, descriptors);
        for (idx, w) in witnesses.into_iter().enumerate() {
            tx.input[idx].witness = Witness::from_vec(w);
        }
        Ok(tx)
    }
    fn get_sweep_address(&self) -> Address {
        self.sweep_address.clone()
    }
}
