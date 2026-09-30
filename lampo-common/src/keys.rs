//! In-memory signer: the default [`LampoSigner`] implementation, backed by
//! LDK's `KeysManager`.
use std::sync::{Arc, OnceLock, Weak};
use std::time::SystemTime;

#[cfg(feature = "unsafe_channel_keys")]
use bitcoin::secp256k1::SecretKey;
use lightning::bolt11_invoice;
use lightning::ln::script::ShutdownScript;
use lightning::sign::{
    ChangeDestinationSourceSync, EntropySource, NodeSigner, OutputSpender, SignerProvider,
};

use crate::ldk::sign::KeysManager;
use crate::signer::{LampoChannelSigner, LampoSigner};
use crate::wallet::WalletManager;

pub struct LampoKeysManager {
    pub(crate) inner: KeysManager,
    /// Weak so the wallet can own this keys manager without a cycle.
    wallet: OnceLock<Weak<dyn WalletManager>>,

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
            wallet: OnceLock::new(),
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

    /// Build a keys manager from a seed, using the current time as the
    /// LDK starting-time entropy.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let start_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock is before the unix epoch");
        Self::new(&seed, start_time.as_secs(), start_time.subsec_nanos())
    }

    /// Like [`Self::from_seed`], but every channel signer uses the five
    /// given base secrets (`/`-separated hex: funding, revocation, payment,
    /// delayed payment, HTLC). The sixth component is ignored: the shachain
    /// seed is drawn fresh per process. Testing only.
    #[cfg(feature = "unsafe_channel_keys")]
    pub fn with_channel_keys(seed: [u8; 32], channels_keys: String) -> Self {
        let keys = channels_keys.split('/').collect::<Vec<_>>();
        let mut manager = Self::from_seed(seed);
        manager.set_channels_keys(
            keys[0].to_string(),
            keys[1].to_string(),
            keys[2].to_string(),
            keys[3].to_string(),
            keys[4].to_string(),
            keys[5].to_string(),
        );
        manager
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

    fn wallet_script(&self) -> Result<bitcoin::ScriptBuf, ()> {
        self.wallet
            .get()
            .and_then(|wallet| wallet.upgrade())
            .ok_or(())?
            .next_wallet_script()
            .map_err(|err| {
                log::error!(target: "lampo-keys", "wallet script: {err}");
            })
    }

    fn destination_script(&self, channel_keys_id: [u8; 32]) -> Result<bitcoin::ScriptBuf, ()> {
        self.wallet_script()
            .or_else(|_| self.inner.get_destination_script(channel_keys_id))
    }
}

impl LampoSigner for LampoKeysManager {
    fn set_wallet(&self, wallet: Arc<dyn WalletManager>) {
        let _ = self.wallet.set(Arc::downgrade(&wallet));
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
    type EcdsaSigner = LampoChannelSigner;

    fn derive_channel_signer(&self, channel_keys_id: [u8; 32]) -> Self::EcdsaSigner {
        #[cfg(feature = "unsafe_channel_keys")]
        if self.funding_key.is_some() {
            use crate::ldk::sign::InMemorySigner;

            // FIXME(vincenzopalazzo): make this a general
            let commitment_seed = [
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
                255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            ];
            // LDK 0.3 split the payment key into v1/v2; with v2 derivation
            // disabled the v1 key is the one in use, so reuse it for both.
            return LampoChannelSigner::new(InMemorySigner::new(
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
            ));
        }
        LampoChannelSigner::new(self.inner.derive_channel_signer(channel_keys_id))
    }

    fn generate_channel_keys_id(&self, inbound: bool, user_channel_id: u128) -> [u8; 32] {
        self.inner
            .generate_channel_keys_id(inbound, user_channel_id)
    }

    fn get_destination_script(&self, channel_keys_id: [u8; 32]) -> Result<bitcoin::ScriptBuf, ()> {
        self.destination_script(channel_keys_id)
    }

    fn get_shutdown_scriptpubkey(&self) -> Result<ShutdownScript, ()> {
        match self.wallet_script() {
            Ok(script) => {
                ShutdownScript::try_from(script).or_else(|_| self.inner.get_shutdown_scriptpubkey())
            }
            Err(()) => self.inner.get_shutdown_scriptpubkey(),
        }
    }
}

impl ChangeDestinationSourceSync for LampoKeysManager {
    fn get_change_destination_script(&self) -> Result<bitcoin::ScriptBuf, ()> {
        self.destination_script([0; 32])
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bitcoin::secp256k1::Secp256k1;
    use lightning::sign::{ChannelSigner, EntropySource, NodeSigner, SignerProvider};

    use super::LampoKeysManager;
    use crate::ldk::sign::KeysManager;
    use crate::signer::{LampoChannelSigner, LampoSigner};

    const SEED: [u8; 32] = [7; 32];

    /// A signer that only delegates, standing in for an external signer.
    /// Proves the trait is object safe and usable through `Arc<dyn>`.
    struct Delegating(LampoKeysManager);

    impl LampoSigner for Delegating {}

    impl EntropySource for Delegating {
        fn get_secure_random_bytes(&self) -> [u8; 32] {
            self.0.get_secure_random_bytes()
        }
    }

    impl NodeSigner for Delegating {
        fn get_expanded_key(&self) -> lightning::ln::inbound_payment::ExpandedKey {
            self.0.get_expanded_key()
        }
        fn get_peer_storage_key(&self) -> lightning::sign::PeerStorageKey {
            self.0.get_peer_storage_key()
        }
        fn get_receive_auth_key(&self) -> lightning::sign::ReceiveAuthKey {
            self.0.get_receive_auth_key()
        }
        fn ecdh(
            &self,
            recipient: lightning::sign::Recipient,
            other_key: &bitcoin::secp256k1::PublicKey,
            tweak: Option<&bitcoin::secp256k1::Scalar>,
        ) -> Result<bitcoin::secp256k1::ecdh::SharedSecret, ()> {
            self.0.ecdh(recipient, other_key, tweak)
        }
        fn get_node_id(
            &self,
            recipient: lightning::sign::Recipient,
        ) -> Result<bitcoin::secp256k1::PublicKey, ()> {
            self.0.get_node_id(recipient)
        }
        fn sign_bolt12_invoice(
            &self,
            invoice: &lightning::offers::invoice::UnsignedBolt12Invoice,
        ) -> Result<bitcoin::secp256k1::schnorr::Signature, ()> {
            self.0.sign_bolt12_invoice(invoice)
        }
        fn sign_gossip_message(
            &self,
            msg: lightning::ln::msgs::UnsignedGossipMessage,
        ) -> Result<bitcoin::secp256k1::ecdsa::Signature, ()> {
            self.0.sign_gossip_message(msg)
        }
        fn sign_invoice(
            &self,
            invoice: &lightning::bolt11_invoice::RawBolt11Invoice,
            recipient: lightning::sign::Recipient,
        ) -> Result<bitcoin::secp256k1::ecdsa::RecoverableSignature, ()> {
            self.0.sign_invoice(invoice, recipient)
        }
        fn sign_message(&self, msg: &[u8]) -> Result<String, ()> {
            self.0.sign_message(msg)
        }
    }

    impl lightning::sign::OutputSpender for Delegating {
        fn spend_spendable_outputs(
            &self,
            descriptors: &[&lightning::sign::SpendableOutputDescriptor],
            outputs: Vec<bitcoin::TxOut>,
            change_destination_script: bitcoin::ScriptBuf,
            feerate_sat_per_1000_weight: u32,
            locktime: Option<bitcoin::absolute::LockTime>,
            secp_ctx: &Secp256k1<bitcoin::secp256k1::All>,
        ) -> Result<bitcoin::Transaction, ()> {
            self.0.spend_spendable_outputs(
                descriptors,
                outputs,
                change_destination_script,
                feerate_sat_per_1000_weight,
                locktime,
                secp_ctx,
            )
        }
    }

    impl SignerProvider for Delegating {
        type EcdsaSigner = LampoChannelSigner;
        fn derive_channel_signer(&self, channel_keys_id: [u8; 32]) -> Self::EcdsaSigner {
            self.0.derive_channel_signer(channel_keys_id)
        }
        fn generate_channel_keys_id(&self, inbound: bool, user_channel_id: u128) -> [u8; 32] {
            self.0.generate_channel_keys_id(inbound, user_channel_id)
        }
        fn get_destination_script(
            &self,
            channel_keys_id: [u8; 32],
        ) -> Result<bitcoin::ScriptBuf, ()> {
            self.0.get_destination_script(channel_keys_id)
        }
        fn get_shutdown_scriptpubkey(&self) -> Result<lightning::ln::script::ShutdownScript, ()> {
            self.0.get_shutdown_scriptpubkey()
        }
    }

    impl lightning::sign::ChangeDestinationSourceSync for Delegating {
        fn get_change_destination_script(&self) -> Result<bitcoin::ScriptBuf, ()> {
            lightning::sign::ChangeDestinationSourceSync::get_change_destination_script(&self.0)
        }
    }

    /// The erased channel signer must derive exactly what LDK's
    /// `KeysManager` derives for the same seed, or existing channels would
    /// silently change keys.
    #[test]
    fn erased_signer_matches_ldk_derivation() {
        let manager = LampoKeysManager::new(&SEED, 1, 2);
        let reference = KeysManager::new(&SEED, 1, 2, false);
        let keys_id = manager.generate_channel_keys_id(false, 42);
        let secp = Secp256k1::new();

        let erased = manager.derive_channel_signer(keys_id);
        let expected = reference.derive_channel_signer(keys_id);

        assert_eq!(erased.channel_keys_id(), expected.channel_keys_id());
        assert_eq!(erased.pubkeys(&secp), expected.pubkeys(&secp));
        assert_eq!(
            erased.get_per_commitment_point(0, &secp),
            expected.get_per_commitment_point(0, &secp)
        );
        assert_eq!(
            manager.get_node_id(lightning::sign::Recipient::Node),
            reference.get_node_id(lightning::sign::Recipient::Node)
        );
    }

    /// Everything LDK needs works through `Arc<dyn LampoSigner>`.
    #[test]
    fn signer_is_usable_as_trait_object() {
        let signer: Arc<dyn LampoSigner> = Arc::new(Delegating(LampoKeysManager::new(&SEED, 1, 2)));
        let reference = LampoKeysManager::new(&SEED, 1, 2);
        let secp = Secp256k1::new();

        let keys_id = signer.generate_channel_keys_id(true, 1);
        let channel_signer = signer.derive_channel_signer(keys_id);
        let cloned = channel_signer.clone();
        assert_eq!(cloned.pubkeys(&secp), channel_signer.pubkeys(&secp));
        assert_eq!(
            channel_signer.pubkeys(&secp),
            reference.derive_channel_signer(keys_id).pubkeys(&secp)
        );
        assert_eq!(
            signer.get_node_id(lightning::sign::Recipient::Node),
            reference.get_node_id(lightning::sign::Recipient::Node)
        );
    }
}
