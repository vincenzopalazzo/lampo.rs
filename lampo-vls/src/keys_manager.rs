use vls_proxy::vls_protocol_client::{DynSigner, KeysManagerClient};
use lightning_signer::invoice::bolt12::UnsignedBolt12Invoice;
use lampo_common::bitcoin::secp256k1::ecdh::SharedSecret;
use lampo_common::bitcoin::absolute::LockTime;
use lampo_common::bitcoin::secp256k1::ecdsa::{RecoverableSignature, Signature as EcdsaSignature};
use lampo_common::bitcoin::secp256k1::schnorr::Signature;
use lampo_common::bitcoin::secp256k1::Secp256k1;
use lampo_common::bitcoin::secp256k1::{PublicKey, Scalar};
use lampo_common::bitcoin::bech32::u5;
use lampo_common::bitcoin::{ScriptBuf, Transaction, TxOut, Witness};
use lampo_common::secp256k1::Signing;
use lampo_common::ldk::ln::{msgs, script::ShutdownScript};
use lampo_common::ldk::offers::invoice_request::UnsignedInvoiceRequest;
use lampo_common::ldk::sign::{OutputSpender, SpendableOutputDescriptor};
use lampo_common::ldk::sign::{EntropySource, NodeSigner, SignerProvider};
use lampo_common::ldk::sign::{KeyMaterial, Recipient};


use crate::util::create_spending_transaction;

#[allow(dead_code)]
/// Holds an instance of KeysManagerClient which interacts with the VLS protocol to fetch or manage keys.
// FIXME: This should be implemented under VLS?
pub struct LampoKeysManager {
    /// The KeysManagerClient is a client-side interface for interacting with the key management functionalities of the VLS signer.
    client: KeysManagerClient,
}

pub trait LampoKeysInterface: NodeSigner + SignerProvider + OutputSpender + EntropySource + Send + Sync {}

impl LampoKeysManager {
    pub fn new(client: KeysManagerClient) -> Self {
        LampoKeysManager {
            client,
        }
    }
}

// To get signer instances for individual channels.
impl SignerProvider for LampoKeysManager {
    type EcdsaSigner = DynSigner;

    fn generate_channel_keys_id(&self, inbound: bool, channel_value_satoshis: u64, user_channel_id: u128,) -> [u8; 32] {
        self.client.generate_channel_keys_id(inbound, channel_value_satoshis, user_channel_id)
    }

    fn derive_channel_signer(&self, channel_value_satoshis: u64, channel_keys_id: [u8; 32],) -> Self::EcdsaSigner {
        let client = self.client.derive_channel_signer(channel_value_satoshis, channel_keys_id);
        DynSigner::new(client)
    }

    fn read_chan_signer(&self, reader: &[u8]) -> Result<Self::EcdsaSigner, msgs::DecodeError> {
        let signer = self.client.read_chan_signer(reader)?;
        Ok(DynSigner::new(signer))
    }

    fn get_shutdown_scriptpubkey(&self) -> Result<ShutdownScript, ()> {
        self.client.get_shutdown_scriptpubkey()
    }

    fn get_destination_script(&self, channel_keys_id: [u8; 32]) -> Result<ScriptBuf, ()> {
        self.client.get_destination_script(channel_keys_id)
    }

}

/// Source of entropy.
impl EntropySource for LampoKeysManager {
    fn get_secure_random_bytes(&self) -> [u8; 32] {
        self.client.get_secure_random_bytes()
    }
}

// Cryptographic operations at the scope level of the Signer
impl NodeSigner for LampoKeysManager {
    fn get_inbound_payment_key_material(&self) -> KeyMaterial {
        self.client.get_inbound_payment_key_material()
    }

    fn get_node_id(&self, recipient: Recipient) -> Result<PublicKey, ()> {
        self.client.get_node_id(recipient)
    }

    fn ecdh(&self, recipient: Recipient, other_key: &PublicKey, tweak: Option<&Scalar>,) -> Result<SharedSecret, ()> {
        self.client.ecdh(recipient, other_key, tweak)
    }

    fn sign_invoice(&self, hrp_bytes: &[u8], invoice_data: &[u5], recipient: Recipient,) -> Result<RecoverableSignature, ()> {
        self.client.sign_invoice(hrp_bytes, invoice_data, recipient)
    }
    fn sign_bolt12_invoice_request( &self, invoice_request: &UnsignedInvoiceRequest,) -> Result<Signature, ()> {
        self.client.sign_bolt12_invoice_request(invoice_request)
    }

    fn sign_bolt12_invoice(&self, invoice: &UnsignedBolt12Invoice) -> Result<Signature, ()> {
        self.client.sign_bolt12_invoice(invoice)
    }
    fn sign_gossip_message(&self, msg: msgs::UnsignedGossipMessage) -> Result<EcdsaSignature, ()> {
        self.client.sign_gossip_message(msg)
    }
}

impl OutputSpender for LampoKeysManager {
    fn spend_spendable_outputs<C: Signing>(&self, descriptors: &[&SpendableOutputDescriptor], outputs: Vec<TxOut>, change_destination_script: ScriptBuf, feerate_sat_per_1000_weight: u32, _locktime: Option<LockTime>, _secp_ctx: &Secp256k1<C>,) -> Result<Transaction, ()> {
        let mut tx = create_spending_transaction(descriptors, outputs, Box::new(change_destination_script), feerate_sat_per_1000_weight).unwrap();
        let witnesses = self.client.sign_onchain_tx(&tx, descriptors);
        for(idx, w) in witnesses.into_iter().enumerate() {
            tx.input[idx].witness = Witness::from_vec(w);
        }
        Ok(tx)
    }
}
