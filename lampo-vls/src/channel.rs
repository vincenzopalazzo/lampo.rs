//! Per-channel signer over a dedicated hsmd fd.
//!
//! Mirrors `SignerClient` in `vls-protocol-client`, including the lazy
//! `SetupChannel`: LDK 0.2 removed `provide_channel_parameters`, so the
//! channel parameters reach the signer on the first signing call, and an
//! inbound `validate_holder_commitment` that arrives before that is deferred
//! and replayed once the channel is set up.
use std::sync::{Arc, Mutex, Weak};

use lampo_common::bitcoin::secp256k1::ecdsa::Signature;
use lampo_common::bitcoin::secp256k1::{All, PublicKey, Secp256k1, SecretKey};
use lampo_common::bitcoin::{Transaction, Txid};
use lampo_common::ldk::ln::chan_utils::{
    ChannelPublicKeys, ChannelTransactionParameters, ClosingTransaction, CommitmentTransaction,
    HTLCOutputInCommitment, HolderCommitmentTransaction,
};
use lampo_common::ldk::ln::msgs::UnsignedChannelAnnouncement;
use lampo_common::ldk::sign::ecdsa::EcdsaChannelSigner;
use lampo_common::ldk::sign::{ChannelSigner, HTLCDescriptor};
use lampo_common::ldk::types::payment::PaymentPreimage;
use lampo_common::ldk::util::ser::Writeable;
use lampo_common::wallet::WalletManager;
use vls_protocol::model::{Basepoints, DisclosedSecret};
use vls_protocol::msgs;
use vls_protocol::serde_bolt::{Array, ArrayBE, Octets, WithSize};

use crate::convert::{
    channel_type_bytes, commitment_number, from_pubkey, from_signature, keys_id_from_dbid,
    to_bitcoin_sig, to_htlcs, to_pubkey,
};
use crate::transport::Conn;

/// Path the signer derives from the allowlisted keychain xpubs to verify the
/// local mutual-close script: just the address index, because the signer's
/// own-wallet check runs first and errors on any multi-component path. Falls
/// back to the reference client's hint when the script is not ours.
fn wallet_path_hint(wallet: &Option<Weak<dyn WalletManager>>, script: &[u8]) -> ArrayBE<u32> {
    let script = lampo_common::bitcoin::ScriptBuf::from_bytes(script.to_vec());
    match wallet
        .as_ref()
        .and_then(Weak::upgrade)
        .and_then(|wallet| wallet.script_derivation(&script))
    {
        Some((_keychain, index)) => vec![index].into(),
        None => vec![1].into(),
    }
}

struct SetupState {
    done: bool,
    deferred_validate: Option<msgs::ValidateCommitmentTx2>,
}

struct Inner {
    conn: Conn,
    dbid: u64,
    pubkeys: ChannelPublicKeys,
    setup: Mutex<SetupState>,
    wallet: Option<Weak<dyn WalletManager>>,
}

/// Cloneable handle; every clone shares the fd and the setup state, which
/// is what LDK expects when it re-derives a signer for the same channel.
#[derive(Clone)]
pub struct VlsChannelSigner {
    inner: Arc<Inner>,
}

impl VlsChannelSigner {
    pub fn new(
        conn: Conn,
        dbid: u64,
        pubkeys: ChannelPublicKeys,
        wallet: Option<Weak<dyn WalletManager>>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                conn,
                dbid,
                pubkeys,
                setup: Mutex::new(SetupState {
                    done: false,
                    deferred_validate: None,
                }),
                wallet,
            }),
        }
    }

    fn call<T: msgs::SerBolt, R: msgs::DeBolt>(&self, msg: T) -> Result<R, ()> {
        self.inner.conn.call(msg).map_err(|err| {
            log::error!(target: "lampo-vls", "channel {} signer call failed: {err}", self.inner.dbid);
        })
    }

    /// Send `SetupChannel` once, then replay a deferred validation. The lock
    /// is held across both so no other call signs against an unset channel.
    /// On failure nothing is marked done, so a later call retries.
    fn ensure_channel_setup(&self, params: &ChannelTransactionParameters) -> Result<(), ()> {
        let mut state = self.inner.setup.lock().unwrap_or_else(|e| e.into_inner());
        if state.done {
            return Ok(());
        }
        self.setup_channel(params)?;
        if let Some(message) = state.deferred_validate.clone() {
            let _: msgs::ValidateCommitmentTxReply = self.call(message)?;
            state.deferred_validate = None;
        }
        state.done = true;
        Ok(())
    }

    fn setup_channel(&self, params: &ChannelTransactionParameters) -> Result<(), ()> {
        let funding = params.funding_outpoint.ok_or(())?;
        let counterparty = params.counterparty_parameters.as_ref().ok_or(())?;
        let message = msgs::SetupChannel {
            is_outbound: params.is_outbound_from_holder,
            channel_value: params.channel_value_satoshis,
            push_value: 0,
            funding_txid: funding.txid,
            funding_txout: funding.index,
            to_self_delay: params.holder_selected_contest_delay,
            local_shutdown_script: Octets::EMPTY,
            local_shutdown_wallet_index: None,
            remote_basepoints: Basepoints {
                revocation: to_pubkey(&counterparty.pubkeys.revocation_basepoint.0),
                payment: to_pubkey(&counterparty.pubkeys.payment_point),
                htlc: to_pubkey(&counterparty.pubkeys.htlc_basepoint.0),
                delayed_payment: to_pubkey(&counterparty.pubkeys.delayed_payment_basepoint.0),
            },
            remote_funding_pubkey: to_pubkey(&counterparty.pubkeys.funding_pubkey),
            remote_to_self_delay: counterparty.selected_contest_delay,
            remote_shutdown_script: Octets::EMPTY,
            channel_type: channel_type_bytes(&params.channel_type_features).into(),
        };
        let _: msgs::SetupChannelReply = self.call(message)?;
        Ok(())
    }

    fn unsupported(&self, what: &str) -> Result<Signature, ()> {
        log::error!(
            target: "lampo-vls",
            "channel {}: {what} is not supported by the VLS signer yet",
            self.inner.dbid
        );
        Err(())
    }
}

impl ChannelSigner for VlsChannelSigner {
    fn get_per_commitment_point(
        &self,
        idx: u64,
        _secp_ctx: &Secp256k1<All>,
    ) -> Result<PublicKey, ()> {
        let reply: msgs::GetPerCommitmentPoint2Reply = self.call(msgs::GetPerCommitmentPoint2 {
            commitment_number: commitment_number(idx),
        })?;
        from_pubkey(&reply.point)
    }

    fn release_commitment_secret(&self, idx: u64) -> Result<[u8; 32], ()> {
        // Asking for the point at idx + 2 releases the secret at idx. This
        // relies on the HsmdInit2 protocol version (4), where the secret is
        // still returned here.
        let reply: msgs::GetPerCommitmentPointReply = self.call(msgs::GetPerCommitmentPoint {
            commitment_number: commitment_number(idx) + 2,
        })?;
        reply.secret.map(|secret| secret.0).ok_or_else(|| {
            log::error!(target: "lampo-vls", "channel {}: signer withheld the commitment secret", self.inner.dbid);
        })
    }

    fn validate_holder_commitment(
        &self,
        holder_tx: &HolderCommitmentTransaction,
        _outbound_htlc_preimages: Vec<PaymentPreimage>,
    ) -> Result<(), ()> {
        let tx = holder_tx.trust();
        let message = msgs::ValidateCommitmentTx2 {
            commitment_number: commitment_number(tx.commitment_number()),
            feerate: tx.negotiated_feerate_per_kw(),
            to_local_value_sat: tx.to_broadcaster_value_sat(),
            to_remote_value_sat: tx.to_countersignatory_value_sat(),
            htlcs: to_htlcs(tx.nondust_htlcs(), false),
            signature: to_bitcoin_sig(&holder_tx.counterparty_sig),
            htlc_signatures: Array(
                holder_tx
                    .counterparty_htlc_sigs
                    .iter()
                    .map(to_bitcoin_sig)
                    .collect(),
            ),
        };
        {
            let mut state = self.inner.setup.lock().unwrap_or_else(|e| e.into_inner());
            if !state.done {
                // One slot only: overwriting would drop a commitment the
                // signer never saw. Failing closes the channel instead.
                if state.deferred_validate.is_some() {
                    log::error!(target: "lampo-vls", "channel {}: second holder commitment before setup", self.inner.dbid);
                    return Err(());
                }
                state.deferred_validate = Some(message);
                return Ok(());
            }
        }
        let _: msgs::ValidateCommitmentTxReply = self.call(message)?;
        Ok(())
    }

    fn validate_counterparty_revocation(&self, idx: u64, secret: &SecretKey) -> Result<(), ()> {
        let _: msgs::ValidateRevocationReply = self.call(msgs::ValidateRevocation {
            commitment_number: commitment_number(idx),
            commitment_secret: DisclosedSecret(secret.secret_bytes()),
        })?;
        Ok(())
    }

    fn pubkeys(&self, _secp_ctx: &Secp256k1<All>) -> ChannelPublicKeys {
        self.inner.pubkeys.clone()
    }

    fn new_funding_pubkey(
        &self,
        _splice_parent_funding_txid: Txid,
        _secp_ctx: &Secp256k1<All>,
    ) -> PublicKey {
        // Splicing is unsupported upstream (VLS #538). The signer refuses to
        // sign the splice, so returning the current key only makes the
        // negotiation fail at signing instead of here.
        log::error!(target: "lampo-vls", "channel {}: splicing is not supported by the VLS signer", self.inner.dbid);
        self.inner.pubkeys.funding_pubkey
    }

    fn channel_keys_id(&self) -> [u8; 32] {
        keys_id_from_dbid(self.inner.dbid)
    }
}

impl EcdsaChannelSigner for VlsChannelSigner {
    fn sign_counterparty_commitment(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        commitment_tx: &CommitmentTransaction,
        _inbound_htlc_preimages: Vec<PaymentPreimage>,
        _outbound_htlc_preimages: Vec<PaymentPreimage>,
        _secp_ctx: &Secp256k1<All>,
    ) -> Result<(Signature, Vec<Signature>), ()> {
        self.ensure_channel_setup(channel_parameters)?;
        let tx = commitment_tx.trust();
        // Values are from the counterparty's point of view.
        let reply: msgs::SignCommitmentTxWithHtlcsReply =
            self.call(msgs::SignRemoteCommitmentTx2 {
                remote_per_commitment_point: to_pubkey(&tx.keys().per_commitment_point),
                commitment_number: commitment_number(tx.commitment_number()),
                feerate: tx.negotiated_feerate_per_kw(),
                to_local_value_sat: tx.to_countersignatory_value_sat(),
                to_remote_value_sat: tx.to_broadcaster_value_sat(),
                htlcs: to_htlcs(tx.nondust_htlcs(), true),
            })?;
        let signature = from_signature(&reply.signature.signature)?;
        let htlc_signatures = reply
            .htlc_signatures
            .iter()
            .map(|sig| from_signature(&sig.signature))
            .collect::<Result<Vec<_>, ()>>()?;
        Ok((signature, htlc_signatures))
    }

    fn sign_holder_commitment(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        commitment_tx: &HolderCommitmentTransaction,
        _secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.ensure_channel_setup(channel_parameters)?;
        let reply: msgs::SignCommitmentTxReply = self.call(msgs::SignLocalCommitmentTx2 {
            commitment_number: commitment_number(commitment_tx.commitment_number()),
        })?;
        from_signature(&reply.signature.signature)
    }

    #[cfg(feature = "unsafe_channel_keys")]
    fn unsafe_sign_holder_commitment(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        commitment_tx: &HolderCommitmentTransaction,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.sign_holder_commitment(channel_parameters, commitment_tx, secp_ctx)
    }

    fn sign_justice_revoked_output(
        &self,
        _channel_parameters: &ChannelTransactionParameters,
        _justice_tx: &Transaction,
        _input: usize,
        _amount: u64,
        _per_commitment_key: &SecretKey,
        _secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        // FIXME: map onto SignPenaltyToUs (14); blocks the mainnet gate.
        self.unsupported("sign_justice_revoked_output")
    }

    fn sign_justice_revoked_htlc(
        &self,
        _channel_parameters: &ChannelTransactionParameters,
        _justice_tx: &Transaction,
        _input: usize,
        _amount: u64,
        _per_commitment_key: &SecretKey,
        _htlc: &HTLCOutputInCommitment,
        _secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        // FIXME: map onto SignPenaltyToUs (14); blocks the mainnet gate.
        self.unsupported("sign_justice_revoked_htlc")
    }

    fn sign_holder_htlc_transaction(
        &self,
        htlc_tx: &Transaction,
        input: usize,
        htlc_descriptor: &HTLCDescriptor,
        _secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        let htlc = &htlc_descriptor.htlc;
        let reply: msgs::SignTxReply = self.call(msgs::SignLocalHtlcTx2 {
            tx: WithSize(htlc_tx.clone()),
            input: input as u32,
            per_commitment_number: htlc_descriptor.per_commitment_number,
            offered: htlc.offered,
            cltv_expiry: htlc.cltv_expiry,
            htlc_amount_msat: htlc.amount_msat,
            payment_hash: vls_protocol::model::Sha256(htlc.payment_hash.0),
        })?;
        from_signature(&reply.signature.signature)
    }

    fn sign_counterparty_htlc_transaction(
        &self,
        _channel_parameters: &ChannelTransactionParameters,
        _htlc_tx: &Transaction,
        _input: usize,
        _amount: u64,
        _per_commitment_point: &PublicKey,
        _htlc: &HTLCOutputInCommitment,
        _secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        // FIXME: map onto SignRemoteHtlcToUs (13); blocks the mainnet gate.
        self.unsupported("sign_counterparty_htlc_transaction")
    }

    fn sign_closing_transaction(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        closing_tx: &ClosingTransaction,
        _secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.ensure_channel_setup(channel_parameters)?;
        let local_script = closing_tx.to_holder_script().to_bytes();
        let local_wallet_path_hint = wallet_path_hint(&self.inner.wallet, &local_script);
        let reply: msgs::SignTxReply = self.call(msgs::SignMutualCloseTx2 {
            to_local_value_sat: closing_tx.to_holder_value_sat(),
            to_remote_value_sat: closing_tx.to_counterparty_value_sat(),
            local_script: local_script.into(),
            remote_script: closing_tx.to_counterparty_script().to_bytes().into(),
            local_wallet_path_hint,
        })?;
        from_signature(&reply.signature.signature)
    }

    fn sign_holder_keyed_anchor_input(
        &self,
        _channel_parameters: &ChannelTransactionParameters,
        _anchor_tx: &Transaction,
        _input: usize,
        _secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        // FIXME: map onto SignAnchorspend (147).
        self.unsupported("sign_holder_keyed_anchor_input")
    }

    fn sign_channel_announcement_with_funding_key(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        msg: &UnsignedChannelAnnouncement,
        _secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.ensure_channel_setup(channel_parameters)?;
        // CLN hands hsmd the full wire message; 258 zero bytes stand in for
        // the two signatures and the length prefix it would carry.
        let mut announcement = vec![0u8; 258];
        announcement.extend(msg.encode());
        let reply: msgs::SignChannelAnnouncementReply =
            self.call(msgs::SignChannelAnnouncement {
                announcement: announcement.into(),
            })?;
        from_signature(&reply.bitcoin_signature)
    }

    fn sign_splice_shared_input(
        &self,
        _channel_parameters: &ChannelTransactionParameters,
        _tx: &Transaction,
        _input_index: usize,
        _secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.unsupported("sign_splice_shared_input")
    }
}
