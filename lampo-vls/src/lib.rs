//! Validating Lightning Signer backend for lampo.
//!
//! Keys live in `vlsd`; lampo talks to it through `remote_hsmd_socket` using
//! the CLN hsmd wire protocol (`vls-protocol` messages). Every LDK signing
//! call is a blocking request/reply on a UNIX socket, which fits LDK's
//! synchronous signer traits. See `docs/designs/vls-hsmd-signer.md`.
//!
//! Status: open, pay, cooperative close and sweep work. Penalty and
//! counterparty-HTLC claims are not mapped yet, so [`VlsSigner::spawn`]
//! refuses mainnet.
pub mod channel;
pub mod convert;
pub mod state;
pub mod transport;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use lampo_common::bitcoin::bip32::{ChildNumber, DerivationPath, Xpub};
use lampo_common::bitcoin::constants::genesis_block;
use lampo_common::bitcoin::secp256k1::ecdh::SharedSecret;
use lampo_common::bitcoin::secp256k1::ecdsa::{RecoverableSignature, RecoveryId, Signature};
use lampo_common::bitcoin::secp256k1::{schnorr, All, PublicKey, Scalar, Secp256k1};
use lampo_common::bitcoin::{absolute::LockTime, Network, ScriptBuf, Transaction, TxOut, Witness};
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::ldk::bolt11_invoice::RawBolt11Invoice;
use lampo_common::ldk::ln::inbound_payment::ExpandedKey;
use lampo_common::ldk::ln::msgs::UnsignedGossipMessage;
use lampo_common::ldk::ln::script::ShutdownScript;
use lampo_common::ldk::offers::invoice::UnsignedBolt12Invoice;
use lampo_common::ldk::sign::{
    ChangeDestinationSourceSync, EntropySource, NodeSigner, OutputSpender, PeerStorageKey,
    ReceiveAuthKey, Recipient, SignerProvider, SpendableOutputDescriptor,
};
use lampo_common::ldk::util::ser::Writeable;
use lampo_common::signer::{LampoChannelSigner, LampoSigner};
use lampo_common::wallet::WalletManager;
use vls_protocol::model::{Bip32KeyVersion, CloseInfo, PubKey, Utxo};
use vls_protocol::msgs;
use vls_protocol::psbt::StreamedPSBT;
use vls_protocol::serde_bolt::{Array, Octets};

use crate::channel::VlsChannelSigner;
use crate::convert::{
    dbid_from_keys_id, encode_signed_message, from_pubkey, from_signature, keys_id_from_dbid,
    to_pubkey,
};
use crate::state::SignerState;
use crate::transport::{Conn, Proxy};

/// hsmd wire version we pin. Version 4 is the last where
/// `GetPerCommitmentPoint` still releases the old secret, which
/// `release_commitment_secret` relies on; 5+ moves it to `RevokeCommitmentTx`.
const HSM_WIRE_VERSION: u32 = 4;
/// `DeriveSecret` info strings for the two LDK keys `HsmdInit` does not
/// return. Seed-derived on the signer, so stable across restarts.
const INBOUND_PAYMENT_KEY_INFO: &[u8] = b"lampo/inbound_payment_key";
const PEER_STORAGE_KEY_INFO: &[u8] = b"lampo/peer_storage_key";
/// LDK gives `derive_channel_signer` no peer, so every channel is keyed by
/// `(zero peer, dbid)` on the signer, exactly as VLS's own LDK client does.
const NO_PEER: [u8; 33] = [0u8; 33];

/// What `VlsSigner::spawn` needs. Built from `LampoConf` by
/// [`VlsSignerConfig::from_conf`].
pub struct VlsSignerConfig {
    /// Path to the `remote_hsmd_socket` binary.
    pub proxy_bin: PathBuf,
    pub network: Network,
    /// bitcoind RPC URL the proxy's chain frontend follows, with credentials.
    pub bitcoind_rpc_url: String,
    /// Port the proxy listens on for `vlsd` to dial in.
    pub port: u16,
    /// Directory for the node-side state file.
    pub datadir: PathBuf,
}

impl VlsSignerConfig {
    pub fn from_conf(conf: &LampoConf) -> error::Result<Self> {
        let proxy_bin = conf
            .vls_proxy_bin
            .clone()
            .ok_or_else(|| error::anyhow!("`signer=vls` needs `vls-proxy-bin`"))?;
        let url = conf.core_url.as_deref().ok_or_else(|| {
            error::anyhow!("`signer=vls` needs `core-url` for the chain frontend")
        })?;
        let bitcoind_rpc_url = match (&conf.core_user, &conf.core_pass) {
            (Some(user), Some(pass)) => {
                let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
                format!("{scheme}://{user}:{pass}@{rest}")
            }
            _ => url.to_string(),
        };
        Ok(Self {
            proxy_bin: PathBuf::from(proxy_bin),
            network: conf.network,
            bitcoind_rpc_url,
            port: conf.vls_port.unwrap_or(7701),
            datadir: PathBuf::from(conf.path()),
        })
    }
}

/// [`LampoSigner`] backed by VLS.
pub struct VlsSigner {
    _proxy: Proxy,
    node_id: PublicKey,
    xpub: Xpub,
    expanded_key: ExpandedKey,
    peer_storage_key: PeerStorageKey,
    receive_auth_key: ReceiveAuthKey,
    state: Mutex<SignerState>,
    /// Channel fds by dbid, so a re-derived signer reuses its connection.
    channels: Mutex<HashMap<u64, VlsChannelSigner>>,
    wallet: OnceLock<Weak<dyn WalletManager>>,
}

impl VlsSigner {
    /// Spawn the proxy, run the `HsmdInit` handshake and load node state.
    ///
    /// `HsmdInit` rather than `HsmdInit2`: the proxy only starts its chain
    /// frontend after `HsmdInit`, and without blocks the signer refuses to
    /// sign any state past the first commitment ("funding is not buried").
    /// Blocks until the signer answers, which requires `vlsd` to have
    /// connected to the proxy.
    pub fn spawn(config: VlsSignerConfig) -> error::Result<Arc<Self>> {
        if config.network == Network::Bitcoin {
            error::bail!(
                "the VLS signer cannot run on mainnet yet: penalty and counterparty HTLC \
                 claims are not implemented, see docs/designs/vls-hsmd-signer.md"
            );
        }
        let network_name = network_name(config.network);
        let envs = [
            ("VLS_NETWORK", network_name.to_string()),
            ("BITCOIND_RPC_URL", config.bitcoind_rpc_url.clone()),
            ("VLS_PORT", config.port.to_string()),
            ("VLS_BIND", "127.0.0.1".to_string()),
        ];
        let proxy = Proxy::spawn(&config.proxy_bin, &config.datadir.join("vls-proxy"), &envs)?;

        log::info!(target: "lampo-vls", "waiting for vlsd on port {}", config.port);
        let init: msgs::HsmdInitReplyV4 = proxy.root().call(msgs::HsmdInit {
            key_version: bip32_key_version(config.network),
            chain_params: genesis_block(config.network).block_hash(),
            encryption_key: None,
            dev_privkey: None,
            dev_bip32_seed: None,
            dev_channel_secrets: None,
            dev_channel_secrets_shaseed: None,
            hsm_wire_min_version: HSM_WIRE_VERSION,
            hsm_wire_max_version: HSM_WIRE_VERSION,
        })?;
        if init.hsm_version != HSM_WIRE_VERSION {
            error::bail!(
                "signer negotiated hsm wire version {}, lampo needs {HSM_WIRE_VERSION}",
                init.hsm_version
            );
        }
        let node_id = from_pubkey(&init.node_id)
            .map_err(|_| error::anyhow!("signer returned an invalid node id"))?;
        let xpub = Xpub::decode(&init.bip32.0)?;
        log::info!(target: "lampo-vls", "signer node id {node_id}");
        let derive = |info: &[u8]| -> error::Result<[u8; 32]> {
            let reply: msgs::DeriveSecretReply = proxy.root().call(msgs::DeriveSecret {
                info: Octets(info.to_vec()),
            })?;
            Ok(reply.secret.0)
        };
        let inbound_payment_key = derive(INBOUND_PAYMENT_KEY_INFO)?;
        let peer_storage_key = derive(PEER_STORAGE_KEY_INFO)?;

        let state_path = config.datadir.join("vls-signer.json");
        let state = SignerState::load_or_create(&state_path, || {
            let reply: Result<msgs::GetSecureRandomBytesReply, _> =
                proxy.root().call(msgs::GetSecureRandomBytes {});
            reply
                .expect("signer must answer GetSecureRandomBytes right after init")
                .random_bytes
                .0
        })?;

        Ok(Arc::new(Self {
            _proxy: proxy,
            node_id,
            xpub,
            expanded_key: ExpandedKey::new(inbound_payment_key),
            peer_storage_key: PeerStorageKey {
                inner: peer_storage_key,
            },
            receive_auth_key: ReceiveAuthKey(state.receive_auth_key),
            state: Mutex::new(state),
            channels: Mutex::new(HashMap::new()),
            wallet: OnceLock::new(),
        }))
    }

    fn root(&self) -> &Conn {
        self._proxy.root()
    }

    /// The signer's own BIP32 account key, useful to allowlist or inspect.
    pub fn xpub(&self) -> &Xpub {
        &self.xpub
    }

    fn wallet_script(&self) -> Result<ScriptBuf, ()> {
        let wallet = self
            .wallet
            .get()
            .and_then(|wallet| wallet.upgrade())
            .ok_or_else(|| {
                log::error!(target: "lampo-vls", "no wallet attached to source destination scripts");
            })?;
        wallet.next_wallet_script().map_err(|err| {
            log::error!(target: "lampo-vls", "wallet script: {err}");
        })
    }

    fn channel_signer(&self, dbid: u64) -> Result<VlsChannelSigner, transport::Error> {
        let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(signer) = channels.get(&dbid) {
            return Ok(signer.clone());
        }
        // NewChannel and GetChannelBasepoints are node-level messages: the
        // signer's per-channel handler rejects them (and vlsd aborts). Only
        // the signing calls travel on the channel fd.
        let _: msgs::NewChannelReply = self.root().call(msgs::NewChannel {
            peer_id: PubKey(NO_PEER),
            dbid,
        })?;
        let basepoints: msgs::GetChannelBasepointsReply =
            self.root().call(msgs::GetChannelBasepoints {
                node_id: PubKey(NO_PEER),
                dbid,
            })?;
        let conn = self.root().open_channel(PubKey(NO_PEER), dbid)?;
        let pubkeys = lampo_common::ldk::ln::chan_utils::ChannelPublicKeys {
            funding_pubkey: from_pubkey(&basepoints.funding).map_err(invalid_key)?,
            revocation_basepoint: from_pubkey(&basepoints.basepoints.revocation)
                .map_err(invalid_key)?
                .into(),
            payment_point: from_pubkey(&basepoints.basepoints.payment).map_err(invalid_key)?,
            delayed_payment_basepoint: from_pubkey(&basepoints.basepoints.delayed_payment)
                .map_err(invalid_key)?
                .into(),
            htlc_basepoint: from_pubkey(&basepoints.basepoints.htlc)
                .map_err(invalid_key)?
                .into(),
        };
        let signer = VlsChannelSigner::new(conn, dbid, pubkeys, self.wallet.get().cloned());
        channels.insert(dbid, signer.clone());
        Ok(signer)
    }

    fn descriptor_to_utxo(descriptor: &SpendableOutputDescriptor) -> Utxo {
        let (outpoint, amount, keyindex, close_info) = match descriptor {
            // Mutual close output on the destination script.
            SpendableOutputDescriptor::StaticOutput {
                outpoint, output, ..
            } => (*outpoint, output.value, 1, None),
            // We force-closed: delayed output to us.
            SpendableOutputDescriptor::DelayedPaymentOutput(o) => (
                o.outpoint,
                o.output.value,
                0,
                Some(CloseInfo {
                    channel_id: dbid_from_keys_id(&o.channel_keys_id),
                    peer_id: PubKey(NO_PEER),
                    commitment_point: Some(to_pubkey(&o.per_commitment_point)),
                    is_anchors: false,
                    csv: u32::from(o.to_self_delay),
                }),
            ),
            // They force-closed: non-delayed output to us.
            SpendableOutputDescriptor::StaticPaymentOutput(o) => (
                o.outpoint,
                o.output.value,
                0,
                Some(CloseInfo {
                    channel_id: dbid_from_keys_id(&o.channel_keys_id),
                    peer_id: PubKey(NO_PEER),
                    commitment_point: None,
                    is_anchors: false,
                    csv: 0,
                }),
            ),
        };
        Utxo {
            txid: outpoint.txid,
            outnum: u32::from(outpoint.index),
            amount: amount.to_sat(),
            keyindex,
            is_p2sh: false,
            script: Octets::EMPTY,
            close_info,
            // FIXME: LDK does not tell us; matters only for coinbase-funded channels.
            is_in_coinbase: false,
        }
    }
}

/// Allowlist entries that let the signer accept wallet-owned scripts.
///
/// One xpub per BDK keychain (`account/0` external, `account/1` change), not
/// the account xpub: the signer first tries its own wallet with the given
/// path and errors on any path longer than one component, so lampo can only
/// send the address index, and each keychain xpub derives it in one step.
pub fn wallet_allowlist(account: &Xpub) -> Vec<String> {
    let secp = Secp256k1::verification_only();
    [0u32, 1]
        .into_iter()
        .filter_map(|keychain| {
            let child = ChildNumber::from_normal_idx(keychain).ok()?;
            account.ckd_pub(&secp, child).ok()
        })
        .map(|xpub| format!("xpub:{xpub}"))
        .collect()
}

impl VlsSigner {
    /// Tell the signer which sweep outputs pay to our wallet: it derives the
    /// allowlisted keychain xpubs at each output's path and rejects unknown
    /// outputs under `policy-onchain-no-unknown-outputs`.
    fn annotate_wallet_outputs(
        &self,
        psbt: &mut lampo_common::bitcoin::Psbt,
        secp_ctx: &Secp256k1<All>,
    ) {
        let Some(wallet) = self.wallet.get().and_then(|wallet| wallet.upgrade()) else {
            return;
        };
        let Some(xpub) = wallet.account_xpub() else {
            return;
        };
        for (index, output) in psbt.unsigned_tx.output.iter().enumerate() {
            let Some((keychain, child)) = wallet.script_derivation(&output.script_pubkey) else {
                continue;
            };
            let (Ok(keychain), Ok(child)) = (
                ChildNumber::from_normal_idx(keychain),
                ChildNumber::from_normal_idx(child),
            ) else {
                continue;
            };
            let Ok(keychain_xpub) = xpub.ckd_pub(secp_ctx, keychain) else {
                continue;
            };
            let path = DerivationPath::from(vec![child]);
            let Ok(derived) = keychain_xpub.derive_pub(secp_ctx, &path) else {
                continue;
            };
            psbt.outputs[index]
                .bip32_derivation
                .insert(derived.public_key, (keychain_xpub.fingerprint(), path));
        }
    }
}

fn invalid_key(_: ()) -> transport::Error {
    transport::Error::Message("signer returned an invalid channel basepoint".into())
}

/// BIP32 version bytes CLN sends in `HsmdInit`.
fn bip32_key_version(network: Network) -> Bip32KeyVersion {
    match network {
        Network::Bitcoin => Bip32KeyVersion {
            pubkey_version: 0x0488_b21e,
            privkey_version: 0x0488_ade4,
        },
        _ => Bip32KeyVersion {
            pubkey_version: 0x0435_87cf,
            privkey_version: 0x0435_8394,
        },
    }
}

fn network_name(network: Network) -> &'static str {
    match network {
        Network::Bitcoin => "bitcoin",
        Network::Testnet => "testnet",
        Network::Signet => "signet",
        Network::Regtest => "regtest",
        _ => "regtest",
    }
}

impl LampoSigner for VlsSigner {
    fn set_wallet(&self, wallet: Arc<dyn WalletManager>) {
        match wallet.account_xpub() {
            Some(xpub) => log::info!(
                target: "lampo-vls",
                "wallet-owned close and sweep scripts need these vlsd allowlist entries: {}",
                wallet_allowlist(&xpub).join(", ")
            ),
            None => log::warn!(
                target: "lampo-vls",
                "wallet exposes no account xpub: the signer will reject closes and sweeps to it"
            ),
        }
        let _ = self.wallet.set(Arc::downgrade(&wallet));
    }
}

impl EntropySource for VlsSigner {
    fn get_secure_random_bytes(&self) -> [u8; 32] {
        let reply: msgs::GetSecureRandomBytesReply = self
            .root()
            .call(msgs::GetSecureRandomBytes {})
            .expect("signer must provide secure random bytes");
        reply.random_bytes.0
    }
}

impl NodeSigner for VlsSigner {
    fn get_expanded_key(&self) -> ExpandedKey {
        self.expanded_key
    }

    fn get_peer_storage_key(&self) -> PeerStorageKey {
        self.peer_storage_key
    }

    fn get_receive_auth_key(&self) -> ReceiveAuthKey {
        self.receive_auth_key
    }

    fn ecdh(
        &self,
        recipient: Recipient,
        other_key: &PublicKey,
        tweak: Option<&Scalar>,
    ) -> Result<SharedSecret, ()> {
        if !matches!(recipient, Recipient::Node) || tweak.is_some() {
            log::error!(target: "lampo-vls", "ecdh: phantom nodes and tweaks are unsupported");
            return Err(());
        }
        let reply: msgs::EcdhReply = self
            .root()
            .call(msgs::Ecdh {
                point: to_pubkey(other_key),
            })
            .map_err(log_err)?;
        Ok(SharedSecret::from_bytes(reply.secret.0))
    }

    fn get_node_id(&self, recipient: Recipient) -> Result<PublicKey, ()> {
        match recipient {
            Recipient::Node => Ok(self.node_id),
            Recipient::PhantomNode => Err(()),
        }
    }

    fn sign_bolt12_invoice(
        &self,
        invoice: &UnsignedBolt12Invoice,
    ) -> Result<schnorr::Signature, ()> {
        let mut bytes = Vec::new();
        invoice.write(&mut bytes).map_err(|_| ())?;
        let reply: msgs::SignBolt12InvoiceReply = self
            .root()
            .call(msgs::SignBolt12Invoice {
                invoice_bytes: Octets(bytes),
            })
            .map_err(log_err)?;
        schnorr::Signature::from_slice(&reply.signature.0).map_err(|_| ())
    }

    fn sign_gossip_message(&self, msg: UnsignedGossipMessage) -> Result<Signature, ()> {
        let reply: msgs::SignGossipMessageReply = self
            .root()
            .call(msgs::SignGossipMessage {
                message: Octets(msg.encode()),
            })
            .map_err(log_err)?;
        from_signature(&reply.signature)
    }

    fn sign_invoice(
        &self,
        invoice: &RawBolt11Invoice,
        recipient: Recipient,
    ) -> Result<RecoverableSignature, ()> {
        if !matches!(recipient, Recipient::Node) {
            return Err(());
        }
        let (hrp, data) = invoice.to_raw();
        let reply: msgs::SignInvoiceReply = self
            .root()
            .call(msgs::SignInvoice {
                u5bytes: Octets(data.iter().map(|fe| fe.to_u8()).collect()),
                hrp: Octets(hrp.into_bytes()),
            })
            .map_err(log_err)?;
        let recid = RecoveryId::from_i32(i32::from(reply.signature.0[64])).map_err(|_| ())?;
        RecoverableSignature::from_compact(&reply.signature.0[..64], recid).map_err(|_| ())
    }

    fn sign_message(&self, msg: &[u8]) -> Result<String, ()> {
        let reply: msgs::SignMessageReply = self
            .root()
            .call(msgs::SignMessage {
                message: Octets(msg.to_vec()),
            })
            .map_err(log_err)?;
        Ok(encode_signed_message(&reply.signature.0))
    }
}

impl SignerProvider for VlsSigner {
    type EcdsaSigner = LampoChannelSigner;

    fn generate_channel_keys_id(&self, _inbound: bool, _user_channel_id: u128) -> [u8; 32] {
        let dbid = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_dbid()
            .expect("cannot persist the VLS dbid counter");
        keys_id_from_dbid(dbid)
    }

    fn derive_channel_signer(&self, channel_keys_id: [u8; 32]) -> Self::EcdsaSigner {
        let dbid = dbid_from_keys_id(&channel_keys_id);
        let signer = self
            .channel_signer(dbid)
            .unwrap_or_else(|err| panic!("cannot reach the VLS signer for channel {dbid}: {err}"));
        LampoChannelSigner::new(signer)
    }

    fn get_destination_script(&self, _channel_keys_id: [u8; 32]) -> Result<ScriptBuf, ()> {
        self.wallet_script()
    }

    fn get_shutdown_scriptpubkey(&self) -> Result<ShutdownScript, ()> {
        ShutdownScript::try_from(self.wallet_script()?).map_err(|_| ())
    }
}

impl OutputSpender for VlsSigner {
    fn spend_spendable_outputs(
        &self,
        descriptors: &[&SpendableOutputDescriptor],
        outputs: Vec<TxOut>,
        change_destination_script: ScriptBuf,
        feerate_sat_per_1000_weight: u32,
        locktime: Option<LockTime>,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<Transaction, ()> {
        let (mut psbt, _expected_weight) =
            SpendableOutputDescriptor::create_spendable_outputs_psbt(
                secp_ctx,
                descriptors,
                outputs,
                change_destination_script,
                feerate_sat_per_1000_weight,
                locktime,
            )?;
        self.annotate_wallet_outputs(&mut psbt, secp_ctx);
        let mut tx = psbt.unsigned_tx.clone();
        let utxos = Array(
            descriptors
                .iter()
                .map(|d| Self::descriptor_to_utxo(d))
                .collect(),
        );
        let reply: msgs::SignWithdrawalReply = self
            .root()
            .call(msgs::SignWithdrawal {
                utxos,
                psbt: StreamedPSBT::new(psbt).into(),
            })
            .map_err(log_err)?;
        let signed = reply.psbt.0.inner;
        if signed.inputs.len() != tx.input.len() {
            log::error!(target: "lampo-vls", "signer returned a PSBT with the wrong input count");
            return Err(());
        }
        for (input, signed) in tx.input.iter_mut().zip(signed.inputs) {
            let witness = signed.final_script_witness.ok_or_else(|| {
                log::error!(target: "lampo-vls", "signer left a sweep input unsigned");
            })?;
            input.witness = Witness::from_slice(&witness.to_vec());
        }
        Ok(tx)
    }
}

impl ChangeDestinationSourceSync for VlsSigner {
    fn get_change_destination_script(&self) -> Result<ScriptBuf, ()> {
        self.wallet_script()
    }
}

fn log_err(err: transport::Error) {
    log::error!(target: "lampo-vls", "signer call failed: {err}");
}

/// Helper for embedders: where the node-side state file lives.
pub fn state_path(datadir: &Path) -> PathBuf {
    datadir.join("vls-signer.json")
}
