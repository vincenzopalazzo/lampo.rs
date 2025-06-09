use std::{sync::Arc, time::SystemTime};

use bitcoin::secp256k1::{Secp256k1, SecretKey};
use bitcoin::ScriptBuf;
use lightning::sign::{ChannelSigner, InMemorySigner, NodeSigner, OutputSpender, SignerProvider};
use lightning::types::payment::PaymentPreimage;
use tokio::sync::Mutex;

use crate::ldk::invoice;
use crate::ldk::sign::ecdsa::EcdsaChannelSigner;
use crate::ldk::sign::{EntropySource, KeysManager};
use crate::wallet::WalletManager;

/// Lampo keys implementations
pub struct LampoKeys {
    pub keys_manager: Arc<LampoKeysManager>,
}

impl LampoKeys {
    pub fn new(seed: [u8; 32]) -> Self {
        // Fill in random_32_bytes with secure random data, or, on restart, reload the seed from disk.
        let start_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();

        LampoKeys {
            keys_manager: Arc::new(LampoKeysManager::new(
                &seed,
                start_time.as_secs(),
                start_time.subsec_nanos(),
            )),
        }
    }

    pub async fn with_wallet_manager(&self, wallet_manager: Arc<dyn WalletManager>) {
        self.keys_manager.with_wallet_manager(wallet_manager).await;
    }

    // FIXME: add this under a feature flag
    pub fn with_channel_keys(seed: [u8; 32], channels_keys: String) -> Self {
        // Fill in random_32_bytes with secure random data, or, on restart, reload the seed from disk.
        let start_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();

        let keys = channels_keys.split('/').collect::<Vec<_>>();

        let mut manager =
            LampoKeysManager::new(&seed, start_time.as_secs(), start_time.subsec_nanos());
        manager.set_channels_keys(
            keys[0].to_string(),
            keys[1].to_string(),
            keys[2].to_string(),
            keys[3].to_string(),
            keys[4].to_string(),
            keys[5].to_string(),
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
    pub(crate) inner: KeysManager,

    funding_key: Option<SecretKey>,
    revocation_base_secret: Option<SecretKey>,
    payment_base_secret: Option<SecretKey>,
    delayed_payment_base_secret: Option<SecretKey>,
    htlc_base_secret: Option<SecretKey>,
    shachain_seed: Option<[u8; 32]>,

    channel_signer: std::sync::Mutex<Option<InMemorySigner>>,
    channel_parameters: Mutex<Option<lightning::ln::chan_utils::ChannelTransactionParameters>>,
    /// For customizing the funding transaction we will need to access
    /// the wallet manager if we want to customize the funding transaction
    /// with some special additional information.
    ///
    /// E.g: Allowing ARK factory channels!
    wallet_manager: Mutex<Option<Arc<dyn WalletManager>>>,
}

impl LampoKeysManager {
    pub fn new(seed: &[u8; 32], starting_time_secs: u64, starting_time_nanos: u32) -> Self {
        let inner = KeysManager::new(seed, starting_time_secs, starting_time_nanos);
        Self {
            inner,
            funding_key: None,
            revocation_base_secret: None,
            payment_base_secret: None,
            delayed_payment_base_secret: None,
            htlc_base_secret: None,
            shachain_seed: None,
            channel_signer: std::sync::Mutex::new(None),
            wallet_manager: Mutex::new(None),
            channel_parameters: Mutex::new(None),
        }
    }

    pub async fn with_wallet_manager(&self, wallet_manager: Arc<dyn WalletManager>) {
        *self.wallet_manager.lock().await = Some(wallet_manager);
    }

    // FIXME: put this under a debug a feature flag like `unsafe_channel_keys`
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

    fn get_inbound_payment_key(&self) -> lightning::ln::inbound_payment::ExpandedKey {
        self.inner.get_inbound_payment_key()
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

    fn sign_gossip_message(
        &self,
        msg: lightning::ln::msgs::UnsignedGossipMessage,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.inner.sign_gossip_message(msg)
    }

    fn sign_invoice(
        &self,
        invoice: &invoice::RawBolt11Invoice,
        recipient: lightning::sign::Recipient,
    ) -> Result<bitcoin::secp256k1::ecdsa::RecoverableSignature, ()> {
        self.inner.sign_invoice(invoice, recipient)
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

pub struct LampoChannelSigner {
    signer: InMemorySigner,
    channel_parameters: Option<lightning::ln::chan_utils::ChannelTransactionParameters>,
    pubkeys: lightning::ln::chan_utils::ChannelPublicKeys,

    wallet_manager: Arc<dyn WalletManager>,
}

impl LampoChannelSigner {
    pub fn new(signer: InMemorySigner, wallet_manager: Arc<dyn WalletManager>) -> Self {
        let channel_parameters = signer.get_channel_parameters().map(|p| p.clone());
        let pubkeys = signer.pubkeys().clone();
        Self {
            signer,
            channel_parameters,
            wallet_manager,
            pubkeys,
        }
    }
}

impl ChannelSigner for LampoChannelSigner {
    fn get_funding_spk(&self) -> ScriptBuf {
        // Access the wallet manager without blocking_lock
        let rt = tokio::runtime::Handle::current();
        let wallet_manager_guard = rt.block_on(async { self.wallet_manager.clone() });

        let channel_parameters = self
            .signer
            .get_channel_parameters()
            .map(|p| p.clone())
            .expect("Channel parameters not set");

        wallet_manager_guard
            .build_funding_transaction(&channel_parameters)
            .unwrap()
    }

    fn get_per_commitment_point(
        &self,
        idx: u64,
        secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::secp256k1::PublicKey, ()> {
        self.signer.get_per_commitment_point(idx, secp_ctx)
    }

    fn validate_holder_commitment(
        &self,
        _commitment_tx: &lightning::ln::chan_utils::HolderCommitmentTransaction,
        _preimages: Vec<PaymentPreimage>,
        _secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<(), ()> {
        self.signer
            .validate_holder_commitment(_commitment_tx, _preimages, _secp_ctx)
    }

    fn release_commitment_secret(&self, _idx: u64) -> Result<[u8; 32], ()> {
        self.signer.release_commitment_secret(_idx)
    }

    fn validate_counterparty_revocation(
        &self,
        _idx: u64,
        _secret: &bitcoin::secp256k1::SecretKey,
    ) -> Result<(), ()> {
        self.signer.validate_counterparty_revocation(_idx, _secret)
    }

    fn pubkeys(&self) -> &lightning::ln::chan_utils::ChannelPublicKeys {
        &self.pubkeys
    }

    fn channel_keys_id(&self) -> [u8; 32] {
        self.signer.channel_keys_id()
    }

    fn provide_channel_parameters(
        &mut self,
        channel_parameters: &lightning::ln::chan_utils::ChannelTransactionParameters,
    ) {
        self.signer.provide_channel_parameters(channel_parameters);
    }

    // New required methods
    fn provide_counterparty_parameters(
        &mut self,
        channel_parameters: &lightning::ln::chan_utils::ChannelTransactionParameters,
    ) {
        self.signer
            .provide_counterparty_parameters(channel_parameters);
    }

    fn provide_funding_outpoint(
        &mut self,
        channel_parameters: &lightning::ln::chan_utils::ChannelTransactionParameters,
    ) {
        self.signer.provide_funding_outpoint(channel_parameters);
    }

    fn get_channel_parameters(
        &self,
    ) -> Option<&lightning::ln::chan_utils::ChannelTransactionParameters> {
        self.signer.get_channel_parameters()
    }

    fn get_channel_value_satoshis(&self) -> u64 {
        self.signer.get_channel_value_satoshis()
    }

    fn punish_revokeable_output(
        &self,
        _spending_tx: &bitcoin::Transaction,
        _input: usize,
        _amount: u64,
        _per_commitment_key: &bitcoin::secp256k1::SecretKey,
        _secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
        _revocation_pubkey: &bitcoin::secp256k1::PublicKey,
    ) -> Result<bitcoin::Witness, ()> {
        self.signer.punish_revokeable_output(
            _spending_tx,
            _input,
            _amount,
            _per_commitment_key,
            _secp_ctx,
            _revocation_pubkey,
        )
    }

    fn punish_htlc_output(
        &self,
        _spending_tx: &bitcoin::Transaction,
        _input: usize,
        _amount: u64,
        _per_commitment_key: &bitcoin::secp256k1::SecretKey,
        _secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
        _revocation_pubkey: &bitcoin::secp256k1::PublicKey,
        _htlc: &lightning::ln::chan_utils::HTLCOutputInCommitment,
    ) -> Result<bitcoin::Witness, ()> {
        self.signer.punish_htlc_output(
            _spending_tx,
            _input,
            _amount,
            _per_commitment_key,
            _secp_ctx,
            _revocation_pubkey,
            _htlc,
        )
    }

    fn sweep_counterparty_htlc_output(
        &self,
        _spending_tx: &bitcoin::Transaction,
        _input: usize,
        _amount: u64,
        _secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
        _per_commitment_point: &bitcoin::secp256k1::PublicKey,
        _htlc: &lightning::ln::chan_utils::HTLCOutputInCommitment,
        _preimage: Option<&PaymentPreimage>,
    ) -> Result<bitcoin::Witness, ()> {
        self.signer.sweep_counterparty_htlc_output(
            _spending_tx,
            _input,
            _amount,
            _secp_ctx,
            _per_commitment_point,
            _htlc,
            _preimage,
        )
    }

    fn sign_holder_commitment(
        &self,
        _commitment_tx: &lightning::ln::chan_utils::HolderCommitmentTransaction,
        _secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::Witness, ()> {
        self.signer
            .sign_holder_commitment(_commitment_tx, _secp_ctx)
    }

    fn sign_holder_htlc_transaction(
        &self,
        _htlc_tx: &bitcoin::Transaction,
        _input: usize,
        _htlc_descriptor: &lightning::sign::HTLCDescriptor,
        _secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::Witness, ()> {
        self.signer
            .sign_holder_htlc_transaction(_htlc_tx, _input, _htlc_descriptor, _secp_ctx)
    }

    fn spend_holder_anchor_output(
        &self,
        _anchor_tx: &bitcoin::Transaction,
        _input: usize,
        _secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::Witness, ()> {
        self.signer
            .spend_holder_anchor_output(_anchor_tx, _input, _secp_ctx)
    }

    fn sign_closing_transaction(
        &self,
        _closing_tx: &lightning::ln::chan_utils::ClosingTransaction,
        _secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.signer.sign_closing_transaction(_closing_tx, _secp_ctx)
    }
}

impl EcdsaChannelSigner for LampoChannelSigner {
    fn sign_counterparty_commitment(
        &self,
        commitment_tx: &lightning::ln::chan_utils::CommitmentTransaction,
        inbound_htlc_preimages: Vec<PaymentPreimage>,
        outbound_htlc_preimages: Vec<PaymentPreimage>,
        secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<
        (
            bitcoin::secp256k1::ecdsa::Signature,
            Vec<bitcoin::secp256k1::ecdsa::Signature>,
        ),
        (),
    > {
        self.signer.sign_counterparty_commitment(
            commitment_tx,
            inbound_htlc_preimages,
            outbound_htlc_preimages,
            secp_ctx,
        )
    }

    fn sign_channel_announcement_with_funding_key(
        &self,
        msg: &lightning::ln::msgs::UnsignedChannelAnnouncement,
        secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.signer
            .sign_channel_announcement_with_funding_key(msg, secp_ctx)
    }

    fn sign_splicing_funding_input(
        &self,
        tx: &bitcoin::Transaction,
        input_index: usize,
        input_value: u64,
        secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
    ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
        self.signer
            .sign_splicing_funding_input(tx, input_index, input_value, secp_ctx)
    }
}

impl SignerProvider for LampoKeysManager {
    type EcdsaSigner = LampoChannelSigner;

    fn derive_channel_signer(
        &self,
        channel_value_satoshis: u64,
        channel_keys_id: [u8; 32],
    ) -> Self::EcdsaSigner {
        let signer = if self.funding_key.is_some() {
            // FIXME(vincenzopalazzo): make this a general
            let commitment_seed = [
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            ];
            InMemorySigner::new(
                &Secp256k1::new(),
                self.funding_key.unwrap(),
                self.revocation_base_secret.unwrap(),
                self.payment_base_secret.unwrap(),
                self.delayed_payment_base_secret.unwrap(),
                self.htlc_base_secret.unwrap(),
                commitment_seed,
                channel_value_satoshis,
                channel_keys_id,
                self.shachain_seed.unwrap(),
            )
        } else {
            self.inner
                .derive_channel_signer(channel_value_satoshis, channel_keys_id)
        };

        // Get wallet manager safely with runtime blocking
        let wallet_manager = self.wallet_manager.blocking_lock().clone().unwrap();
        LampoChannelSigner::new(signer, wallet_manager)
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
        let inner_signer = self.inner.read_chan_signer(reader)?;

        // Get wallet manager safely with runtime blocking
        let rt = tokio::runtime::Handle::current();
        let wallet_manager =
            rt.block_on(async { self.wallet_manager.lock().await.clone().unwrap() });

        Ok(LampoChannelSigner::new(inner_signer, wallet_manager))
    }
}
