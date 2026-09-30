//! Signer seam.
//!
//! Every LDK component that needs keys receives an `Arc<dyn LampoSigner>`,
//! so the signer can be swapped (in-memory keys today, a remote or
//! validating signer tomorrow) without touching the node. Channel signers
//! are type-erased behind [`LampoChannelSigner`] because LDK pins them
//! through the `SignerProvider::EcdsaSigner` associated type.
//!
//! This mirrors LDK's own `DynSigner`/`DynKeysInterface`, which upstream only
//! ships under its test-utils feature.
use std::sync::Arc;

use bitcoin::secp256k1::ecdsa::Signature;
use bitcoin::secp256k1::{All, PublicKey, Secp256k1, SecretKey};
use bitcoin::{ScriptBuf, Transaction, Txid};
use lightning::ln::chan_utils::{
    ChannelPublicKeys, ChannelTransactionParameters, ClosingTransaction, CommitmentTransaction,
    HTLCOutputInCommitment, HolderCommitmentTransaction,
};
use lightning::ln::msgs::UnsignedChannelAnnouncement;
use lightning::sign::ecdsa::EcdsaChannelSigner;
use lightning::sign::{
    ChangeDestinationSource, ChangeDestinationSourceSync, ChannelSigner, EntropySource,
    HTLCDescriptor, NodeSigner, OutputSpender, SignerProvider,
};
use lightning::types::payment::PaymentPreimage;

use crate::wallet::WalletManager;

/// The signer lampo hands to LDK.
///
/// Implementations must answer synchronously: lampo never calls
/// `signer_unblocked`, so an `Err` from a signing method stalls the channel
/// instead of being retried. Every LDK generic slot that needs keys
/// (`ChannelManager`, `ChainMonitor`, `OutputSweeper`, routers, the peer
/// manager) is satisfied by `Arc<dyn LampoSigner>`.
pub trait LampoSigner:
    EntropySource
    + NodeSigner
    + SignerProvider<EcdsaSigner = LampoChannelSigner>
    + OutputSpender
    + ChangeDestinationSourceSync
    + Send
    + Sync
{
    /// Attach the on-chain wallet so destination, shutdown and change
    /// scripts are wallet-owned and closed-channel funds land there.
    ///
    /// Default no-op for signers that source those scripts elsewhere.
    fn set_wallet(&self, _wallet: Arc<dyn WalletManager>) {}
}

/// Object-safe view of an LDK channel signer, plus cloning through the box.
pub trait ErasedChannelSigner: EcdsaChannelSigner + Send + Sync {
    fn box_clone(&self) -> Box<dyn ErasedChannelSigner>;
}

impl<T> ErasedChannelSigner for T
where
    T: EcdsaChannelSigner + Clone + Send + Sync + 'static,
{
    fn box_clone(&self) -> Box<dyn ErasedChannelSigner> {
        Box::new(self.clone())
    }
}

/// Type-erased channel signer used as `SignerProvider::EcdsaSigner`.
///
/// Channel monitors never serialize the signer; on restore LDK re-derives it
/// through `SignerProvider::derive_channel_signer`, so erasing the concrete
/// type leaves the on-disk format untouched.
pub struct LampoChannelSigner(Box<dyn ErasedChannelSigner>);

impl LampoChannelSigner {
    pub fn new<S>(signer: S) -> Self
    where
        S: EcdsaChannelSigner + Clone + Send + Sync + 'static,
    {
        Self(Box::new(signer))
    }
}

impl Clone for LampoChannelSigner {
    fn clone(&self) -> Self {
        Self(self.0.box_clone())
    }
}

impl ChannelSigner for LampoChannelSigner {
    fn get_per_commitment_point(
        &self,
        idx: u64,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<PublicKey, ()> {
        self.0.get_per_commitment_point(idx, secp_ctx)
    }

    fn release_commitment_secret(&self, idx: u64) -> Result<[u8; 32], ()> {
        self.0.release_commitment_secret(idx)
    }

    fn validate_holder_commitment(
        &self,
        holder_tx: &HolderCommitmentTransaction,
        outbound_htlc_preimages: Vec<PaymentPreimage>,
    ) -> Result<(), ()> {
        self.0
            .validate_holder_commitment(holder_tx, outbound_htlc_preimages)
    }

    fn validate_counterparty_revocation(&self, idx: u64, secret: &SecretKey) -> Result<(), ()> {
        self.0.validate_counterparty_revocation(idx, secret)
    }

    fn pubkeys(&self, secp_ctx: &Secp256k1<All>) -> ChannelPublicKeys {
        self.0.pubkeys(secp_ctx)
    }

    fn new_funding_pubkey(
        &self,
        splice_parent_funding_txid: Txid,
        secp_ctx: &Secp256k1<All>,
    ) -> PublicKey {
        self.0
            .new_funding_pubkey(splice_parent_funding_txid, secp_ctx)
    }

    fn channel_keys_id(&self) -> [u8; 32] {
        self.0.channel_keys_id()
    }
}

impl EcdsaChannelSigner for LampoChannelSigner {
    fn sign_counterparty_commitment(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        commitment_tx: &CommitmentTransaction,
        inbound_htlc_preimages: Vec<PaymentPreimage>,
        outbound_htlc_preimages: Vec<PaymentPreimage>,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<(Signature, Vec<Signature>), ()> {
        self.0.sign_counterparty_commitment(
            channel_parameters,
            commitment_tx,
            inbound_htlc_preimages,
            outbound_htlc_preimages,
            secp_ctx,
        )
    }

    fn sign_holder_commitment(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        commitment_tx: &HolderCommitmentTransaction,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.0
            .sign_holder_commitment(channel_parameters, commitment_tx, secp_ctx)
    }

    // Only exists when `lightning/_test_utils` is on, which the
    // `unsafe_channel_keys` feature enables.
    #[cfg(feature = "unsafe_channel_keys")]
    fn unsafe_sign_holder_commitment(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        commitment_tx: &HolderCommitmentTransaction,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.0
            .unsafe_sign_holder_commitment(channel_parameters, commitment_tx, secp_ctx)
    }

    fn sign_justice_revoked_output(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        justice_tx: &Transaction,
        input: usize,
        amount: u64,
        per_commitment_key: &SecretKey,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.0.sign_justice_revoked_output(
            channel_parameters,
            justice_tx,
            input,
            amount,
            per_commitment_key,
            secp_ctx,
        )
    }

    fn sign_justice_revoked_htlc(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        justice_tx: &Transaction,
        input: usize,
        amount: u64,
        per_commitment_key: &SecretKey,
        htlc: &HTLCOutputInCommitment,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.0.sign_justice_revoked_htlc(
            channel_parameters,
            justice_tx,
            input,
            amount,
            per_commitment_key,
            htlc,
            secp_ctx,
        )
    }

    fn sign_holder_htlc_transaction(
        &self,
        htlc_tx: &Transaction,
        input: usize,
        htlc_descriptor: &HTLCDescriptor,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.0
            .sign_holder_htlc_transaction(htlc_tx, input, htlc_descriptor, secp_ctx)
    }

    fn sign_counterparty_htlc_transaction(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        htlc_tx: &Transaction,
        input: usize,
        amount: u64,
        per_commitment_point: &PublicKey,
        htlc: &HTLCOutputInCommitment,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.0.sign_counterparty_htlc_transaction(
            channel_parameters,
            htlc_tx,
            input,
            amount,
            per_commitment_point,
            htlc,
            secp_ctx,
        )
    }

    fn sign_closing_transaction(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        closing_tx: &ClosingTransaction,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.0
            .sign_closing_transaction(channel_parameters, closing_tx, secp_ctx)
    }

    fn sign_holder_keyed_anchor_input(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        anchor_tx: &Transaction,
        input: usize,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.0
            .sign_holder_keyed_anchor_input(channel_parameters, anchor_tx, input, secp_ctx)
    }

    fn sign_channel_announcement_with_funding_key(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        msg: &UnsignedChannelAnnouncement,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.0
            .sign_channel_announcement_with_funding_key(channel_parameters, msg, secp_ctx)
    }

    fn sign_splice_shared_input(
        &self,
        channel_parameters: &ChannelTransactionParameters,
        tx: &Transaction,
        input_index: usize,
        secp_ctx: &Secp256k1<All>,
    ) -> Result<Signature, ()> {
        self.0
            .sign_splice_shared_input(channel_parameters, tx, input_index, secp_ctx)
    }
}

/// Adapts the signer's synchronous change destination to the async
/// `ChangeDestinationSource` that `OutputSweeper` requires. That trait
/// returns `impl Future`, so it cannot sit behind `dyn LampoSigner` directly.
pub struct LampoChangeDestination(Arc<dyn LampoSigner>);

impl LampoChangeDestination {
    pub fn new(signer: Arc<dyn LampoSigner>) -> Self {
        Self(signer)
    }
}

impl ChangeDestinationSource for LampoChangeDestination {
    async fn get_change_destination_script(&self) -> Result<ScriptBuf, ()> {
        ChangeDestinationSourceSync::get_change_destination_script(&*self.0)
    }
}
