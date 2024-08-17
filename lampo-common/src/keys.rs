use std::sync::Arc;

use lightning::sign::ecdsa::EcdsaChannelSigner;

use crate::bitcoin::secp256k1::SecretKey;
use crate::ldk::sign::ecdsa::WriteableEcdsaChannelSigner;
use crate::ldk::sign::{EntropySource, KeysManager};
use crate::ldk::sign::{NodeSigner, OutputSpender, SignerProvider};
use crate::ldk::util::ser::Writeable;

/// Lampo keys implementations
pub struct LampoKeys {
    pub keys_manager: Arc<LampoKeysManager>,
}

pub struct LampoSigner {
    pub inner: Arc<dyn WriteableEcdsaChannelSigner>,
}

impl WriteableEcdsaChannelSigner for LampoSigner {}

impl Writeable for LampoSigner {
    fn encode(&self) -> Vec<u8> {
        self.inner.encode()
    }

    fn serialized_length(&self) -> usize {
        self.inner.serialized_length()
    }

    fn write<W: lightning::util::ser::Writer>(&self, writer: &mut W) -> Result<(), std::io::Error> {
        self.inner.write(writer)
    }
}

impl EcdsaChannelSigner for LampoSigner {
    fn sign_channel_announcement_with_funding_key(
        &self,
        msg: &lightning::ln::msgs::UnsignedChannelAnnouncement,
        secp_ctx: &bitcoin::key::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.inner
            .sign_channel_announcement_with_funding_key(msg, secp_ctx)
    }

    fn sign_closing_transaction(
        &self,
        closing_tx: &lightning::ln::chan_utils::ClosingTransaction,
        secp_ctx: &bitcoin::key::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.inner.sign_closing_transaction(closing_tx, secp_ctx)
    }

    fn sign_counterparty_commitment(
        &self,
        commitment_tx: &lightning::ln::chan_utils::CommitmentTransaction,
        inbound_htlc_preimages: Vec<lightning::ln::PaymentPreimage>,
        outbound_htlc_preimages: Vec<lightning::ln::PaymentPreimage>,
        secp_ctx: &bitcoin::key::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<
        (
            bitcoin::secp256k1::ecdsa::Signature,
            Vec<bitcoin::secp256k1::ecdsa::Signature>,
        ),
        (),
    > {
        self.inner.sign_counterparty_commitment(
            commitment_tx,
            inbound_htlc_preimages,
            outbound_htlc_preimages,
            secp_ctx,
        )
    }

    fn sign_counterparty_htlc_transaction(
        &self,
        htlc_tx: &bitcoin::Transaction,
        input: usize,
        amount: u64,
        per_commitment_point: &bitcoin::secp256k1::PublicKey,
        htlc: &lightning::ln::chan_utils::HTLCOutputInCommitment,
        secp_ctx: &bitcoin::key::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.inner.sign_counterparty_htlc_transaction(
            htlc_tx,
            input,
            amount,
            per_commitment_point,
            htlc,
            secp_ctx,
        )
    }

    fn sign_holder_anchor_input(
        &self,
        anchor_tx: &bitcoin::Transaction,
        input: usize,
        secp_ctx: &bitcoin::key::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.inner
            .sign_holder_anchor_input(anchor_tx, input, secp_ctx)
    }

    fn sign_holder_commitment(
        &self,
        commitment_tx: &lightning::ln::chan_utils::HolderCommitmentTransaction,
        secp_ctx: &bitcoin::key::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.inner.sign_holder_commitment(commitment_tx, secp_ctx)
    }

    fn sign_holder_htlc_transaction(
        &self,
        htlc_tx: &bitcoin::Transaction,
        input: usize,
        htlc_descriptor: &lightning::sign::HTLCDescriptor,
        secp_ctx: &bitcoin::key::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.inner
            .sign_holder_htlc_transaction(htlc_tx, input, htlc_descriptor, secp_ctx)
    }

    fn sign_justice_revoked_htlc(
        &self,
        justice_tx: &bitcoin::Transaction,
        input: usize,
        amount: u64,
        per_commitment_key: &SecretKey,
        htlc: &lightning::ln::chan_utils::HTLCOutputInCommitment,
        secp_ctx: &bitcoin::key::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.inner.sign_justice_revoked_htlc(
            justice_tx,
            input,
            amount,
            per_commitment_key,
            htlc,
            secp_ctx,
        )
    }

    fn sign_justice_revoked_output(
        &self,
        justice_tx: &bitcoin::Transaction,
        input: usize,
        amount: u64,
        per_commitment_key: &SecretKey,
        secp_ctx: &bitcoin::key::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.inner.sign_justice_revoked_output(
            justice_tx,
            input,
            amount,
            per_commitment_key,
            secp_ctx,
        )
    }

    fn unsafe_sign_holder_commitment(
        &self,
        commitment_tx: &lightning::ln::chan_utils::HolderCommitmentTransaction,
        secp_ctx: &bitcoin::key::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.unsafe_sign_holder_commitment(commitment_tx, secp_ctx)
    }
}

pub trait ILampoKeys:
    NodeSigner + SignerProvider<EcdsaSigner = LampoKeys> + EntropySource + OutputSpender + Send + Sync
{
}

impl ILampoKeys for KeysManager {}

impl LampoKeys {
    pub fn new(inner: Arc<dyn ILampoKeys>) -> Self {
        LampoKeys {
            keys_manager: Arc::new(LampoKeysManager::new(inner)),
        }
    }

    #[cfg(debug_assertions)]
    pub fn with_channel_keys(inner: Arc<dyn ILampoKeys>, channels_keys: String) -> Self {
        let keys = channels_keys.split('/').collect::<Vec<_>>();

        let mut manager = LampoKeysManager::new(inner);
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
    pub(crate) inner: Arc<dyn ILampoKeys>,

    funding_key: Option<SecretKey>,
    revocation_base_secret: Option<SecretKey>,
    payment_base_secret: Option<SecretKey>,
    delayed_payment_base_secret: Option<SecretKey>,
    htlc_base_secret: Option<SecretKey>,
    shachain_seed: Option<[u8; 32]>,
}

impl LampoKeysManager {
    pub fn new(inner: Arc<dyn ILampoKeys>) -> Self {
        Self {
            inner,
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
    type EcdsaSigner = <dyn ILampoKeys>::EcdsaSigner;

    fn derive_channel_signer(
        &self,
        channel_value_satoshis: u64,
        channel_keys_id: [u8; 32],
    ) -> Self::EcdsaSigner {
        // if self.funding_key.is_some() {
        // // FIXME(vincenzopalazzo): make this a general
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
