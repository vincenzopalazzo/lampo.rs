use lightning_signer::util::loopback::LoopbackSignerKeysInterface;
use vls_proxy::vls_protocol_client::{DynSigner, SpendableKeysInterface};

use lampo_common::bitcoin::bech32::u5;
use lampo_common::bitcoin::consensus::encode::serialize_hex;
use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::bitcoin::{Address, ScriptBuf, Transaction, TxOut};
use lampo_common::error;
use lampo_common::ldk::ln::msgs::{DecodeError, UnsignedGossipMessage};
use lampo_common::ldk::ln::script::ShutdownScript;
use lampo_common::ldk::sign::{
    EntropySource, KeyMaterial, NodeSigner, Recipient, SignerProvider, SpendableOutputDescriptor,
};
use lampo_common::secp256k1::ecdh::SharedSecret;
use lampo_common::secp256k1::ecdsa::{RecoverableSignature, Signature};
use lampo_common::secp256k1::{Scalar, Secp256k1};

pub struct Adapter {
    pub(crate) inner: LoopbackSignerKeysInterface,
    pub(crate) sweep_address: Address,
}

impl SignerProvider for Adapter {
    type EcdsaSigner = DynSigner;

    fn generate_channel_keys_id(
        &self,
        inbound: bool,
        channel_value_satoshis: u64,
        user_channel_id: u128,
    ) -> [u8; 32] {
        self.inner
            .generate_channel_keys_id(inbound, channel_value_satoshis, user_channel_id)
    }

    fn derive_channel_signer(
        &self,
        channel_value_satoshis: u64,
        channel_keys_id: [u8; 32],
    ) -> Self::EcdsaSigner {
        let inner = self
            .inner
            .derive_channel_signer(channel_value_satoshis, channel_keys_id);
        DynSigner {
            inner: Box::new(inner),
        }
    }

    fn read_chan_signer(&self, reader: &[u8]) -> Result<Self::EcdsaSigner, DecodeError> {
        let inner = self.inner.read_chan_signer(reader)?;

        Ok(DynSigner::new(inner))
    }

    fn get_destination_script(&self, channel_keys_id: [u8; 32]) -> Result<ScriptBuf, ()> {
        self.inner.get_destination_script(channel_keys_id)
    }

    fn get_shutdown_scriptpubkey(&self) -> Result<ShutdownScript, ()> {
        self.inner.get_shutdown_scriptpubkey()
    }
}

impl EntropySource for Adapter {
    fn get_secure_random_bytes(&self) -> [u8; 32] {
        self.inner.get_secure_random_bytes()
    }
}

impl NodeSigner for Adapter {
    fn sign_bolt12_invoice(
        &self,
        invoice: &lightning_signer::invoice::bolt12::UnsignedBolt12Invoice,
    ) -> Result<lampo_common::secp256k1::schnorr::Signature, ()> {
        self.inner.sign_bolt12_invoice(invoice)
    }

    fn sign_bolt12_invoice_request(
        &self,
        invoice_request: &lampo_common::ldk::offers::invoice_request::UnsignedInvoiceRequest,
    ) -> Result<lampo_common::secp256k1::schnorr::Signature, ()> {
        self.inner.sign_bolt12_invoice_request(invoice_request)
    }

    fn get_inbound_payment_key_material(&self) -> KeyMaterial {
        self.inner.get_inbound_payment_key_material()
    }

    fn get_node_id(&self, recipient: Recipient) -> Result<PublicKey, ()> {
        match recipient {
            Recipient::Node => {}
            Recipient::PhantomNode => panic!("phantom node not supported"),
        }
        Ok(self.inner.node_id.clone())
    }

    fn ecdh(
        &self,
        recipient: Recipient,
        other_key: &PublicKey,
        tweak: Option<&Scalar>,
    ) -> Result<SharedSecret, ()> {
        self.inner.ecdh(recipient, other_key, tweak)
    }

    fn sign_invoice(
        &self,
        hrp_bytes: &[u8],
        invoice_data: &[u5],
        recipient: Recipient,
    ) -> Result<RecoverableSignature, ()> {
        self.inner.sign_invoice(hrp_bytes, invoice_data, recipient)
    }

    fn sign_gossip_message(&self, msg: UnsignedGossipMessage) -> Result<Signature, ()> {
        self.inner.sign_gossip_message(msg)
    }
}

impl SpendableKeysInterface for Adapter {
    fn spend_spendable_outputs(
        &self,
        descriptors: &[&SpendableOutputDescriptor],
        outputs: Vec<TxOut>,
        change_destination_script: ScriptBuf,
        feerate_sat_per_1000_weight: u32,
        _: &Secp256k1<lampo_common::secp256k1::All>,
    ) -> error::Result<Transaction> {
        let tx = self
            .inner
            .spend_spendable_outputs(
                descriptors,
                outputs,
                change_destination_script,
                feerate_sat_per_1000_weight,
            )
            .map_err(|()| error::anyhow!("failed in spend_spendable_outputs"))?;
        log::info!("spend spendable {}", serialize_hex(&tx));
        Ok(tx)
    }

    fn get_sweep_address(&self) -> Address {
        self.sweep_address.clone()
    }
}
