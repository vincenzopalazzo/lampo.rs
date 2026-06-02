use std::{sync::Arc, time::SystemTime};

#[cfg(feature = "unsafe_channel_keys")]
use bitcoin::secp256k1::SecretKey;
use lightning::bolt11_invoice;
use lightning::sign::{InMemorySigner, NodeSigner, OutputSpender, SignerProvider};

use crate::ldk::sign::{EntropySource, KeysManager};

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

    #[cfg(feature = "unsafe_channel_keys")]
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

    #[cfg(feature = "unsafe_channel_keys")]
    funding_key: Option<SecretKey>,
    #[cfg(feature = "unsafe_channel_keys")]
    revocation_base_secret: Option<SecretKey>,
    #[cfg(feature = "unsafe_channel_keys")]
    payment_base_secret: Option<SecretKey>,
    #[cfg(feature = "unsafe_channel_keys")]
    delayed_payment_base_secret: Option<SecretKey>,
    #[cfg(feature = "unsafe_channel_keys")]
    htlc_base_secret: Option<SecretKey>,
    #[cfg(feature = "unsafe_channel_keys")]
    shachain_seed: Option<[u8; 32]>,
}

impl LampoKeysManager {
    pub fn new(seed: &[u8; 32], starting_time_secs: u64, starting_time_nanos: u32) -> Self {
        // `false` keeps the pre-0.3 (v1) channel key derivation so existing
        // channels keep deriving the same keys after the LDK upgrade.
        let inner = KeysManager::new(seed, starting_time_secs, starting_time_nanos, false);
        Self {
            inner,
            #[cfg(feature = "unsafe_channel_keys")]
            funding_key: None,
            #[cfg(feature = "unsafe_channel_keys")]
            revocation_base_secret: None,
            #[cfg(feature = "unsafe_channel_keys")]
            payment_base_secret: None,
            #[cfg(feature = "unsafe_channel_keys")]
            delayed_payment_base_secret: None,
            #[cfg(feature = "unsafe_channel_keys")]
            htlc_base_secret: None,
            #[cfg(feature = "unsafe_channel_keys")]
            shachain_seed: None,
        }
    }

    #[cfg(feature = "unsafe_channel_keys")]
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
    fn get_expanded_key(&self) -> lightning::ln::inbound_payment::ExpandedKey {
        self.inner.get_expanded_key()
    }

    fn get_peer_storage_key(&self) -> lightning::sign::PeerStorageKey {
        self.inner.get_peer_storage_key()
    }

    fn get_receive_auth_key(&self) -> lightning::sign::ReceiveAuthKey {
        self.inner.get_receive_auth_key()
    }

    fn ecdh(
        &self,
        recipient: lightning::sign::Recipient,
        other_key: &bitcoin::secp256k1::PublicKey,
        tweak: Option<&bitcoin::secp256k1::Scalar>,
    ) -> Result<bitcoin::secp256k1::ecdh::SharedSecret, ()> {
        self.inner.ecdh(recipient, other_key, tweak)
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
        invoice: &bolt11_invoice::RawBolt11Invoice,
        recipient: lightning::sign::Recipient,
    ) -> Result<bitcoin::secp256k1::ecdsa::RecoverableSignature, ()> {
        self.inner.sign_invoice(invoice, recipient)
    }

    fn sign_message(&self, msg: &[u8]) -> Result<String, ()> {
        self.inner.sign_message(msg)
    }
}

impl OutputSpender for LampoKeysManager {
    fn spend_spendable_outputs(
        &self,
        descriptors: &[&lightning::sign::SpendableOutputDescriptor],
        outputs: Vec<bitcoin::TxOut>,
        change_destination_script: bitcoin::ScriptBuf,
        feerate_sat_per_1000_weight: u32,
        locktime: Option<bitcoin::absolute::LockTime>,
        secp_ctx: &bitcoin::secp256k1::Secp256k1<bitcoin::secp256k1::All>,
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
    type EcdsaSigner = InMemorySigner;

    fn derive_channel_signer(&self, channel_keys_id: [u8; 32]) -> Self::EcdsaSigner {
        #[cfg(feature = "unsafe_channel_keys")]
        if self.funding_key.is_some() {
            // FIXME(vincenzopalazzo): make this a general
            let commitment_seed = [
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            ];
            // LDK 0.3 split the payment key into v1/v2; with v2 derivation
            // disabled the v1 key is the one in use, so reuse it for both.
            return InMemorySigner::new(
                self.funding_key.unwrap(),
                self.revocation_base_secret.unwrap(),
                self.payment_base_secret.unwrap(),
                self.payment_base_secret.unwrap(),
                false,
                self.delayed_payment_base_secret.unwrap(),
                self.htlc_base_secret.unwrap(),
                commitment_seed,
                channel_keys_id,
                self.shachain_seed.unwrap(),
            );
        }
        self.inner.derive_channel_signer(channel_keys_id)
    }

    fn generate_channel_keys_id(&self, inbound: bool, user_channel_id: u128) -> [u8; 32] {
        self.inner
            .generate_channel_keys_id(inbound, user_channel_id)
    }

    fn get_destination_script(&self, channel_keys_id: [u8; 32]) -> Result<bitcoin::ScriptBuf, ()> {
        self.inner.get_destination_script(channel_keys_id)
    }

    fn get_shutdown_scriptpubkey(&self) -> Result<lightning::ln::script::ShutdownScript, ()> {
        self.inner.get_shutdown_scriptpubkey()
    }
}
