use core::sync::atomic::{AtomicUsize, Ordering};
use std::ops::Deref;

use crate::chacha20::ChaCha20;
use bitcoin::bech32::u5;
use bitcoin::hashes::sha256::Hash as Sha256;
use bitcoin::hashes::sha256d::Hash as Sha256dHash;
use bitcoin::hashes::{Hash, HashEngine};
use bitcoin::secp256k1::ecdsa::Signature;
use bitcoin::secp256k1::{Message, PublicKey, Secp256k1, Signing};
use bitcoin::util::sighash;
use bitcoin::PublicKey as BitcoinPublicKey;
use bitcoin::{
    blockdata::{opcodes, script::Builder},
    psbt::PartiallySignedTransaction,
    secp256k1::{ecdh::SharedSecret, ecdsa::RecoverableSignature, Scalar, SecretKey},
    util::bip32::{ChildNumber, ExtendedPrivKey, ExtendedPubKey},
    EcdsaSighashType, Network, PackedLockTime, Script, Transaction, TxOut, WPubkeyHash, Witness,
};
use lightning::util::invoice::construct_invoice_preimage;
use lightning::util::ser::ReadableArgs;
use lightning::util::ser::Writeable;
use lightning::{
    ln::{
        msgs::{DecodeError, UnsignedGossipMessage},
        script::ShutdownScript,
    },
    sign::{
        EntropySource, InMemorySigner, KeyMaterial, NodeSigner, Recipient, SignerProvider,
        SpendableOutputDescriptor,
    },
};

#[cfg(not(any(target_pointer_width = "32", target_pointer_width = "64")))]
compile_error!(
    "We need at least 32-bit pointers for atomic counter (and to have enough memory to run LDK)"
);

macro_rules! hash_to_message {
    ($slice: expr) => {
        ::bitcoin::secp256k1::Message::from_slice($slice).unwrap()
    };
}

#[inline]
pub fn sign_with_aux_rand<C: Signing, ES: Deref>(
    ctx: &Secp256k1<C>,
    msg: &Message,
    sk: &SecretKey,
    entropy_source: &ES,
) -> Signature
where
    ES::Target: EntropySource,
{
    let sig = loop {
        let sig = ctx.sign_ecdsa_with_noncedata(msg, sk, &entropy_source.get_secure_random_bytes());
        if sig.serialize_compact()[0] < 0x80 {
            break sig;
        }
    };
    sig
}

/// A simple atomic counter that uses AtomicUsize to give a u64 counter.
pub struct AtomicCounter {
    // Usize needs to be at least 32 bits to avoid overflowing both low and high. If usize is 64
    // bits we will never realistically count into high:
    counter_low: AtomicUsize,
    counter_high: AtomicUsize,
}

impl AtomicCounter {
    pub(crate) fn new() -> Self {
        Self {
            counter_low: AtomicUsize::new(0),
            counter_high: AtomicUsize::new(0),
        }
    }
    pub(crate) fn get_increment(&self) -> u64 {
        let low = self.counter_low.fetch_add(1, Ordering::AcqRel) as u64;
        let high = if low == 0 {
            self.counter_high.fetch_add(1, Ordering::AcqRel) as u64
        } else {
            self.counter_high.load(Ordering::Acquire) as u64
        };
        (high << 32) | low
    }
}

/// Simple implementation of [`EntropySource`], [`NodeSigner`], and [`SignerProvider`] that takes a
/// 32-byte seed for use as a BIP 32 extended key and derives keys from that.
///
/// Your `node_id` is seed/0'.
/// Unilateral closes may use seed/1'.
/// Cooperative closes may use seed/2'.
/// The two close keys may be needed to claim on-chain funds!
///
/// This struct cannot be used for nodes that wish to support receiving phantom payments;
/// [`PhantomKeysManager`] must be used instead.
///
/// Note that switching between this struct and [`PhantomKeysManager`] will invalidate any
/// previously issued invoices and attempts to pay previous invoices will fail.
pub struct KeysManager {
    secp_ctx: Secp256k1<bitcoin::secp256k1::All>,
    node_secret: SecretKey,
    node_id: PublicKey,
    inbound_payment_key: KeyMaterial,
    destination_script: Script,
    shutdown_pubkey: PublicKey,
    channel_master_key: ExtendedPrivKey,
    channel_child_index: AtomicUsize,

    rand_bytes_unique_start: [u8; 32],
    rand_bytes_index: AtomicCounter,

    seed: [u8; 32],
    starting_time_secs: u64,
    starting_time_nanos: u32,

    // The following fields are used only in
    // dev mode to allow user like lnprototest
    // to set a predictable key.
    funding_key: Option<SecretKey>,
    revocation_base_secret: Option<SecretKey>,
    payment_base_secret: Option<SecretKey>,
    delayed_payment_base_secret: Option<SecretKey>,
    htlc_base_secret: Option<SecretKey>,
    shachain_seed: Option<[u8; 32]>,
}

impl KeysManager {
    /// Constructs a [`KeysManager`] from a 32-byte seed. If the seed is in some way biased (e.g.,
    /// your CSRNG is busted) this may panic (but more importantly, you will possibly lose funds).
    /// `starting_time` isn't strictly required to actually be a time, but it must absolutely,
    /// without a doubt, be unique to this instance. ie if you start multiple times with the same
    /// `seed`, `starting_time` must be unique to each run. Thus, the easiest way to achieve this
    /// is to simply use the current time (with very high precision).
    ///
    /// The `seed` MUST be backed up safely prior to use so that the keys can be re-created, however,
    /// obviously, `starting_time` should be unique every time you reload the library - it is only
    /// used to generate new ephemeral key data (which will be stored by the individual channel if
    /// necessary).
    ///
    /// Note that the seed is required to recover certain on-chain funds independent of
    /// [`ChannelMonitor`] data, though a current copy of [`ChannelMonitor`] data is also required
    /// for any channel, and some on-chain during-closing funds.
    ///
    /// [`ChannelMonitor`]: crate::chain::channelmonitor::ChannelMonitor
    pub fn new(seed: &[u8; 32], starting_time_secs: u64, starting_time_nanos: u32) -> Self {
        let secp_ctx = Secp256k1::new();
        // Note that when we aren't serializing the key, network doesn't matter
        match ExtendedPrivKey::new_master(Network::Testnet, seed) {
            Ok(master_key) => {
                let node_secret = master_key
                    .ckd_priv(&secp_ctx, ChildNumber::from_hardened_idx(0).unwrap())
                    .expect("Your RNG is busted")
                    .private_key;
                let node_id = PublicKey::from_secret_key(&secp_ctx, &node_secret);
                let destination_script = match master_key
                    .ckd_priv(&secp_ctx, ChildNumber::from_hardened_idx(1).unwrap())
                {
                    Ok(destination_key) => {
                        let wpubkey_hash = WPubkeyHash::hash(
                            &ExtendedPubKey::from_priv(&secp_ctx, &destination_key)
                                .to_pub()
                                .to_bytes(),
                        );
                        Builder::new()
                            .push_opcode(opcodes::all::OP_PUSHBYTES_0)
                            .push_slice(&wpubkey_hash.into_inner())
                            .into_script()
                    }
                    Err(_) => panic!("Your RNG is busted"),
                };
                let shutdown_pubkey = match master_key
                    .ckd_priv(&secp_ctx, ChildNumber::from_hardened_idx(2).unwrap())
                {
                    Ok(shutdown_key) => {
                        ExtendedPubKey::from_priv(&secp_ctx, &shutdown_key).public_key
                    }
                    Err(_) => panic!("Your RNG is busted"),
                };
                let channel_master_key = master_key
                    .ckd_priv(&secp_ctx, ChildNumber::from_hardened_idx(3).unwrap())
                    .expect("Your RNG is busted");
                let inbound_payment_key: SecretKey = master_key
                    .ckd_priv(&secp_ctx, ChildNumber::from_hardened_idx(5).unwrap())
                    .expect("Your RNG is busted")
                    .private_key;
                let mut inbound_pmt_key_bytes = [0; 32];
                inbound_pmt_key_bytes.copy_from_slice(&inbound_payment_key[..]);

                let mut rand_bytes_engine = Sha256::engine();
                rand_bytes_engine.input(&starting_time_secs.to_be_bytes());
                rand_bytes_engine.input(&starting_time_nanos.to_be_bytes());
                rand_bytes_engine.input(seed);
                rand_bytes_engine.input(b"LDK PRNG Seed");
                let rand_bytes_unique_start = Sha256::from_engine(rand_bytes_engine).into_inner();

                let mut res = KeysManager {
                    secp_ctx,
                    node_secret,
                    node_id,
                    inbound_payment_key: KeyMaterial(inbound_pmt_key_bytes),

                    destination_script,
                    shutdown_pubkey,

                    channel_master_key,
                    channel_child_index: AtomicUsize::new(0),

                    rand_bytes_unique_start,
                    rand_bytes_index: AtomicCounter::new(),

                    seed: *seed,
                    starting_time_secs,
                    starting_time_nanos,

                    funding_key: None,
                    revocation_base_secret: None,
                    delayed_payment_base_secret: None,
                    payment_base_secret: None,
                    htlc_base_secret: None,
                    shachain_seed: None,
                };
                let secp_seed = res.get_secure_random_bytes();
                res.secp_ctx.seeded_randomize(&secp_seed);
                res
            }
            Err(_) => panic!("Your rng is busted"),
        }
    }

    /// Gets the "node_id" secret key used to sign gossip announcements, decode onion data, etc.
    pub fn get_node_secret_key(&self) -> SecretKey {
        self.node_secret
    }

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

        // FIXME: remove the unwrapping and return the error.
        self.funding_key = Some(SecretKey::from_str(&funding_key).unwrap());
        self.revocation_base_secret = Some(SecretKey::from_str(&revocation_base_secret).unwrap());
        self.payment_base_secret = Some(SecretKey::from_str(&payment_base_secret).unwrap());
        self.delayed_payment_base_secret =
            Some(SecretKey::from_str(&delayed_payment_base_secret).unwrap());
        self.htlc_base_secret = Some(SecretKey::from_str(&htlc_base_secret).unwrap());
        // FIXME: check this
        self.shachain_seed = Some(self.get_secure_random_bytes())
    }

    /// Derive an old [`WriteableEcdsaChannelSigner`] containing per-channel secrets based on a key derivation parameters.

    pub fn derive_channel_keys(
        &self,
        channel_value_satoshis: u64,
        params: &[u8; 32],
    ) -> InMemorySigner {
        let chan_id = u64::from_be_bytes(params[0..8].try_into().unwrap());
        let mut unique_start = Sha256::engine();
        unique_start.input(params);
        unique_start.input(&self.seed);

        // We only seriously intend to rely on the channel_master_key for true secure
        // entropy, everything else just ensures uniqueness. We rely on the unique_start (ie
        // starting_time provided in the constructor) to be unique.
        let child_privkey = self
            .channel_master_key
            .ckd_priv(
                &self.secp_ctx,
                ChildNumber::from_hardened_idx((chan_id as u32) % (1 << 31))
                    .expect("key space exhausted"),
            )
            .expect("Your RNG is busted");
        unique_start.input(&child_privkey.private_key[..]);

        let seed = Sha256::from_engine(unique_start).into_inner();

        let commitment_seed = {
            let mut sha = Sha256::engine();
            sha.input(&seed);
            sha.input(&b"commitment seed"[..]);
            Sha256::from_engine(sha).into_inner()
        };
        let funding_key: Option<SecretKey>;
        let revocation_base_key: Option<SecretKey>;
        let payment_key: Option<SecretKey>;
        let delayed_payment_base_key: Option<SecretKey>;
        let htlc_base_key: Option<SecretKey>;
        let prng_seed: Option<[u8; 32]>;

        if self.revocation_base_secret.is_some() {
            #[cfg(not(debug_assertions))]
            compile_error!("this is a bug in the software, we can not have a custom channels keys in a revocation base secret ");
            // the user the custom keys!
            funding_key = self.funding_key;
            revocation_base_key = self.revocation_base_secret;
            payment_key = self.payment_base_secret;
            delayed_payment_base_key = self.delayed_payment_base_secret;
            htlc_base_key = self.htlc_base_secret;
            prng_seed = self.shachain_seed;
        } else {
            macro_rules! key_step {
                ($info: expr, $prev_key: expr) => {{
                    let mut sha = Sha256::engine();
                    sha.input(&seed);
                    sha.input(&$prev_key[..]);
                    sha.input(&$info[..]);
                    SecretKey::from_slice(&Sha256::from_engine(sha).into_inner())
                        .expect("SHA-256 is busted")
                }};
            }
            funding_key = Some(key_step!(b"funding key", commitment_seed));
            revocation_base_key = Some(key_step!(b"revocation base key", funding_key.unwrap()));
            payment_key = Some(key_step!(b"payment key", revocation_base_key.unwrap()));
            delayed_payment_base_key =
                Some(key_step!(b"delayed payment base key", payment_key.unwrap()));
            htlc_base_key = Some(key_step!(
                b"HTLC base key",
                delayed_payment_base_key.unwrap()
            ));
            prng_seed = Some(self.get_secure_random_bytes());
        }
        InMemorySigner::new(
            &self.secp_ctx,
            funding_key.unwrap(),
            revocation_base_key.unwrap(),
            payment_key.unwrap(),
            delayed_payment_base_key.unwrap(),
            htlc_base_key.unwrap(),
            commitment_seed,
            channel_value_satoshis,
            params.clone(),
            prng_seed.unwrap(),
        )
    }

    /// Signs the given [`PartiallySignedTransaction`] which spends the given [`SpendableOutputDescriptor`]s.
    /// The resulting inputs will be finalized and the PSBT will be ready for broadcast if there
    /// are no other inputs that need signing.
    ///
    /// Returns `Err(())` if the PSBT is missing a descriptor or if we fail to sign.
    ///
    /// May panic if the [`SpendableOutputDescriptor`]s were not generated by channels which used
    /// this [`KeysManager`] or one of the [`InMemorySigner`] created by this [`KeysManager`].
    pub fn sign_spendable_outputs_psbt<C: Signing>(
        &self,
        descriptors: &[&SpendableOutputDescriptor],
        psbt: &mut PartiallySignedTransaction,
        secp_ctx: &Secp256k1<C>,
    ) -> Result<(), ()> {
        let mut keys_cache: Option<(InMemorySigner, [u8; 32])> = None;
        for outp in descriptors {
            match outp {
                SpendableOutputDescriptor::StaticPaymentOutput(descriptor) => {
                    let input_idx = psbt
                        .unsigned_tx
                        .input
                        .iter()
                        .position(|i| {
                            i.previous_output == descriptor.outpoint.into_bitcoin_outpoint()
                        })
                        .ok_or(())?;
                    if keys_cache.is_none()
                        || keys_cache.as_ref().unwrap().1 != descriptor.channel_keys_id
                    {
                        keys_cache = Some((
                            self.derive_channel_keys(
                                descriptor.channel_value_satoshis,
                                &descriptor.channel_keys_id,
                            ),
                            descriptor.channel_keys_id,
                        ));
                    }
                    let witness = Witness::from_vec(
                        keys_cache
                            .as_ref()
                            .unwrap()
                            .0
                            .sign_counterparty_payment_input(
                                &psbt.unsigned_tx,
                                input_idx,
                                &descriptor,
                                &secp_ctx,
                            )?,
                    );
                    psbt.inputs[input_idx].final_script_witness = Some(witness);
                }
                SpendableOutputDescriptor::DelayedPaymentOutput(descriptor) => {
                    let input_idx = psbt
                        .unsigned_tx
                        .input
                        .iter()
                        .position(|i| {
                            i.previous_output == descriptor.outpoint.into_bitcoin_outpoint()
                        })
                        .ok_or(())?;
                    if keys_cache.is_none()
                        || keys_cache.as_ref().unwrap().1 != descriptor.channel_keys_id
                    {
                        keys_cache = Some((
                            self.derive_channel_keys(
                                descriptor.channel_value_satoshis,
                                &descriptor.channel_keys_id,
                            ),
                            descriptor.channel_keys_id,
                        ));
                    }
                    let witness = Witness::from_vec(
                        keys_cache.as_ref().unwrap().0.sign_dynamic_p2wsh_input(
                            &psbt.unsigned_tx,
                            input_idx,
                            &descriptor,
                            &secp_ctx,
                        )?,
                    );
                    psbt.inputs[input_idx].final_script_witness = Some(witness);
                }
                SpendableOutputDescriptor::StaticOutput {
                    ref outpoint,
                    ref output,
                } => {
                    let input_idx = psbt
                        .unsigned_tx
                        .input
                        .iter()
                        .position(|i| i.previous_output == outpoint.into_bitcoin_outpoint())
                        .ok_or(())?;
                    let derivation_idx = if output.script_pubkey == self.destination_script {
                        1
                    } else {
                        2
                    };
                    let secret = {
                        // Note that when we aren't serializing the key, network doesn't matter
                        match ExtendedPrivKey::new_master(Network::Testnet, &self.seed) {
                            Ok(master_key) => {
                                match master_key.ckd_priv(
                                    &secp_ctx,
                                    ChildNumber::from_hardened_idx(derivation_idx)
                                        .expect("key space exhausted"),
                                ) {
                                    Ok(key) => key,
                                    Err(_) => panic!("Your RNG is busted"),
                                }
                            }
                            Err(_) => panic!("Your rng is busted"),
                        }
                    };
                    let pubkey = ExtendedPubKey::from_priv(&secp_ctx, &secret).to_pub();
                    if derivation_idx == 2 {
                        assert_eq!(pubkey.inner, self.shutdown_pubkey);
                    }
                    let witness_script =
                        bitcoin::Address::p2pkh(&pubkey, Network::Testnet).script_pubkey();
                    let payment_script = bitcoin::Address::p2wpkh(&pubkey, Network::Testnet)
                        .expect("uncompressed key found")
                        .script_pubkey();

                    if payment_script != output.script_pubkey {
                        return Err(());
                    };

                    let sighash = hash_to_message!(
                        &sighash::SighashCache::new(&psbt.unsigned_tx)
                            .segwit_signature_hash(
                                input_idx,
                                &witness_script,
                                output.value,
                                EcdsaSighashType::All
                            )
                            .unwrap()[..]
                    );
                    let sig = sign_with_aux_rand(secp_ctx, &sighash, &secret.private_key, &self);
                    let mut sig_ser = sig.serialize_der().to_vec();
                    sig_ser.push(EcdsaSighashType::All as u8);
                    let witness =
                        Witness::from_vec(vec![sig_ser, pubkey.inner.serialize().to_vec()]);
                    psbt.inputs[input_idx].final_script_witness = Some(witness);
                }
            }
        }

        Ok(())
    }

    /// Creates a [`Transaction`] which spends the given descriptors to the given outputs, plus an
    /// output to the given change destination (if sufficient change value remains). The
    /// transaction will have a feerate, at least, of the given value.
    ///
    /// The `locktime` argument is used to set the transaction's locktime. If `None`, the
    /// transaction will have a locktime of 0. It it recommended to set this to the current block
    /// height to avoid fee sniping, unless you have some specific reason to use a different
    /// locktime.
    ///
    /// Returns `Err(())` if the output value is greater than the input value minus required fee,
    /// if a descriptor was duplicated, or if an output descriptor `script_pubkey`
    /// does not match the one we can spend.
    ///
    /// We do not enforce that outputs meet the dust limit or that any output scripts are standard.
    ///
    /// May panic if the [`SpendableOutputDescriptor`]s were not generated by channels which used
    /// this [`KeysManager`] or one of the [`InMemorySigner`] created by this [`KeysManager`].
    pub fn spend_spendable_outputs<C: Signing>(
        &self,
        descriptors: &[&SpendableOutputDescriptor],
        outputs: Vec<TxOut>,
        change_destination_script: Script,
        feerate_sat_per_1000_weight: u32,
        locktime: Option<PackedLockTime>,
        secp_ctx: &Secp256k1<C>,
    ) -> Result<Transaction, ()> {
        let (mut psbt, expected_max_weight) =
            SpendableOutputDescriptor::create_spendable_outputs_psbt(
                descriptors,
                outputs,
                change_destination_script,
                feerate_sat_per_1000_weight,
                locktime,
            )?;
        self.sign_spendable_outputs_psbt(descriptors, &mut psbt, secp_ctx)?;

        let spend_tx = psbt.extract_tx();

        debug_assert!(expected_max_weight >= spend_tx.weight());
        // Note that witnesses with a signature vary somewhat in size, so allow
        // `expected_max_weight` to overshoot by up to 3 bytes per input.
        debug_assert!(expected_max_weight <= spend_tx.weight() + descriptors.len() * 3);

        Ok(spend_tx)
    }
}

impl EntropySource for KeysManager {
    fn get_secure_random_bytes(&self) -> [u8; 32] {
        let index = self.rand_bytes_index.get_increment();
        let mut nonce = [0u8; 16];
        nonce[..8].copy_from_slice(&index.to_be_bytes());
        ChaCha20::get_single_block(&self.rand_bytes_unique_start, &nonce)
    }
}

impl NodeSigner for KeysManager {
    fn get_node_id(&self, recipient: Recipient) -> Result<PublicKey, ()> {
        match recipient {
            Recipient::Node => Ok(self.node_id.clone()),
            Recipient::PhantomNode => Err(()),
        }
    }

    fn ecdh(
        &self,
        recipient: Recipient,
        other_key: &PublicKey,
        tweak: Option<&Scalar>,
    ) -> Result<SharedSecret, ()> {
        let mut node_secret = match recipient {
            Recipient::Node => Ok(self.node_secret.clone()),
            Recipient::PhantomNode => Err(()),
        }?;
        if let Some(tweak) = tweak {
            node_secret = node_secret.mul_tweak(tweak).map_err(|_| ())?;
        }
        Ok(SharedSecret::new(other_key, &node_secret))
    }

    fn get_inbound_payment_key_material(&self) -> KeyMaterial {
        self.inbound_payment_key.clone()
    }

    fn sign_invoice(
        &self,
        hrp_bytes: &[u8],
        invoice_data: &[u5],
        recipient: Recipient,
    ) -> Result<RecoverableSignature, ()> {
        let preimage = construct_invoice_preimage(&hrp_bytes, &invoice_data);
        let secret = match recipient {
            Recipient::Node => Ok(&self.node_secret),
            Recipient::PhantomNode => Err(()),
        }?;
        Ok(self
            .secp_ctx
            .sign_ecdsa_recoverable(&hash_to_message!(&Sha256::hash(&preimage)), secret))
    }

    fn sign_gossip_message(&self, msg: UnsignedGossipMessage) -> Result<Signature, ()> {
        let msg_hash = hash_to_message!(&Sha256dHash::hash(&msg.encode()[..])[..]);
        Ok(self.secp_ctx.sign_ecdsa(&msg_hash, &self.node_secret))
    }
}

impl SignerProvider for KeysManager {
    type Signer = InMemorySigner;

    fn generate_channel_keys_id(
        &self,
        _inbound: bool,
        _channel_value_satoshis: u64,
        user_channel_id: u128,
    ) -> [u8; 32] {
        let child_idx = self.channel_child_index.fetch_add(1, Ordering::AcqRel);
        // `child_idx` is the only thing guaranteed to make each channel unique without a restart
        // (though `user_channel_id` should help, depending on user behavior). If it manages to
        // roll over, we may generate duplicate keys for two different channels, which could result
        // in loss of funds. Because we only support 32-bit+ systems, assert that our `AtomicUsize`
        // doesn't reach `u32::MAX`.
        assert!(
            child_idx < core::u32::MAX as usize,
            "2^32 channels opened without restart"
        );
        let mut id = [0; 32];
        id[0..4].copy_from_slice(&(child_idx as u32).to_be_bytes());
        id[4..8].copy_from_slice(&self.starting_time_nanos.to_be_bytes());
        id[8..16].copy_from_slice(&self.starting_time_secs.to_be_bytes());
        id[16..32].copy_from_slice(&user_channel_id.to_be_bytes());
        id
    }

    fn derive_channel_signer(
        &self,
        channel_value_satoshis: u64,
        channel_keys_id: [u8; 32],
    ) -> Self::Signer {
        self.derive_channel_keys(channel_value_satoshis, &channel_keys_id)
    }

    fn read_chan_signer(&self, reader: &[u8]) -> Result<Self::Signer, DecodeError> {
        InMemorySigner::read(&mut std::io::Cursor::new(reader), self)
    }

    fn get_destination_script(&self) -> Result<Script, ()> {
        Ok(self.destination_script.clone())
    }

    fn get_shutdown_scriptpubkey(&self) -> Result<ShutdownScript, ()> {
        let other_publickey = BitcoinPublicKey::new(self.shutdown_pubkey.clone());
        let other_wpubkeyhash = other_publickey.wpubkey_hash().unwrap();
        Ok(ShutdownScript::new_p2wpkh(&other_wpubkeyhash))
    }
}
