//! A [`Persist`] implementation that captures watchtower data.
//!
//! Wraps the filesystem store lampo already uses and, when a tower is
//! configured, extracts the justice (penalty) transaction for every
//! counterparty commitment as monitor updates flow through, following
//! the flow LDK designed for watchtowers:
//!
//! 1. On `commitment_signed` the update carries the new counterparty
//!    commitment: build the unsigned justice transaction and queue it.
//! 2. On `revoke_and_ack` the monitor learns the revocation secret:
//!    queued justice transactions can now be signed, and are moved to
//!    the delivery outbox.
//!
//! Capture happens before the monitor write and never fails the
//! persist call: watchtower trouble must not break channel operation.

use std::sync::{Arc, OnceLock};

use bitcoin::consensus;
use bitcoin::ScriptBuf;
use lightning::chain::chaininterface::{
    ConfirmationTarget, FeeEstimator, FEERATE_FLOOR_SATS_PER_KW,
};
use lightning::chain::chainmonitor::Persist;
use lightning::chain::channelmonitor::{ChannelMonitor, ChannelMonitorUpdate};
use lightning::chain::ChannelMonitorUpdateStatus;
use lightning::ln::chan_utils::CommitmentTransaction;
use lightning::sign::ecdsa::EcdsaChannelSigner;
use lightning::util::persist::MonitorName;
use lightning_persister::fs_store::v1::FilesystemStore;
use tokio::sync::Notify;

use crate::outbox::{Outbox, PendingJustice, SignedJustice};

/// Watchtower runtime state, set once when a tower is configured.
pub(crate) struct WatchtowerCtx {
    pub(crate) outbox: Outbox,
    pub(crate) destination_script: ScriptBuf,
    pub(crate) fee_estimator: Arc<dyn FeeEstimator + Send + Sync>,
    /// Wakes the delivery task when a new appointment hits the outbox.
    pub(crate) notifier: Arc<Notify>,
}

/// The lampo persister: a [`FilesystemStore`] plus optional watchtower
/// capture. With no tower configured it is a pure pass-through.
pub struct WatchtowerPersister {
    kv: Arc<FilesystemStore>,
    ctx: OnceLock<WatchtowerCtx>,
}

impl WatchtowerPersister {
    pub fn new(kv: Arc<FilesystemStore>) -> Self {
        WatchtowerPersister {
            kv,
            ctx: OnceLock::new(),
        }
    }

    /// The underlying store, for the KVStore roles (reading monitors,
    /// the background processor, ...).
    pub fn kv_store(&self) -> Arc<FilesystemStore> {
        self.kv.clone()
    }

    pub(crate) fn enable(&self, ctx: WatchtowerCtx) {
        if self.ctx.set(ctx).is_err() {
            log::warn!(target: "lampo-watchtower", "watchtower already enabled, ignoring");
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.ctx.get().is_some()
    }
}

impl WatchtowerCtx {
    /// Builds the unsigned justice transaction data for a counterparty
    /// commitment, if it has a revokeable output.
    fn form_justice_data(&self, commitment_tx: &CommitmentTransaction) -> Option<PendingJustice> {
        let trusted = commitment_tx.trust();
        let output_idx = trusted.revokeable_output_index()?;
        let value = trusted.built_transaction().transaction.output[output_idx].value;
        let feerate = self
            .fee_estimator
            .get_est_sat_per_1000_weight(ConfirmationTarget::UrgentOnChainSweep)
            .max(FEERATE_FLOOR_SATS_PER_KW) as u64;
        let justice_tx = trusted
            .build_to_local_justice_tx(feerate, self.destination_script.clone())
            .or_else(|_| {
                // The urgent feerate can leave a sub-dust output on
                // small channels: retry at the floor.
                trusted.build_to_local_justice_tx(
                    FEERATE_FLOOR_SATS_PER_KW as u64,
                    self.destination_script.clone(),
                )
            })
            .ok()?;
        Some(PendingJustice {
            justice_tx: consensus::serialize(&justice_tx),
            value_sat: value.to_sat(),
            commitment_number: commitment_tx.commitment_number(),
        })
    }

    /// Queues fresh commitment data and signs whatever the monitor can
    /// already sign, moving it to the delivery outbox.
    fn capture<Signer: EcdsaChannelSigner>(
        &self,
        monitor: &ChannelMonitor<Signer>,
        commitment_txs: Vec<CommitmentTransaction>,
    ) -> crate::error::Result<()> {
        let channel = monitor.channel_id().to_string();
        let mut queue = self.outbox.load_pending(&channel)?;
        let mut changed = false;

        for commitment_tx in &commitment_txs {
            if let Some(justice_data) = self.form_justice_data(commitment_tx) {
                queue.push(justice_data);
                changed = true;
            }
        }

        // Justice transactions sign in commitment order: stop at the
        // first the monitor cannot sign yet (not revoked).
        while let Some(front) = queue.first() {
            let justice_tx = front.tx()?;
            let input_idx = 0;
            let dispute_txid = justice_tx.input[input_idx].previous_output.txid;
            let Ok(signed_tx) = monitor.sign_to_local_justice_tx(
                justice_tx,
                input_idx,
                front.value_sat,
                front.commitment_number,
            ) else {
                break;
            };
            self.outbox.push_signed(&SignedJustice {
                dispute_txid,
                penalty_tx: consensus::serialize(&signed_tx),
                // FIXME: the monitor does not expose the negotiated
                // to_self_delay; current towers ignore the field.
                to_self_delay: 0,
            })?;
            log::debug!(target: "lampo-watchtower", "justice tx for dispute {dispute_txid} queued for the tower");
            queue.remove(0);
            changed = true;
            self.notifier.notify_one();
        }

        if changed {
            self.outbox.store_pending(&channel, &queue)?;
        }
        Ok(())
    }
}

impl<Signer: EcdsaChannelSigner> Persist<Signer> for WatchtowerPersister {
    fn persist_new_channel(
        &self,
        monitor_name: MonitorName,
        monitor: &ChannelMonitor<Signer>,
    ) -> ChannelMonitorUpdateStatus {
        if let Some(ctx) = self.ctx.get() {
            let initial = monitor.initial_counterparty_commitment_tx();
            if let Err(err) = ctx.capture(monitor, initial.into_iter().collect()) {
                log::error!(target: "lampo-watchtower", "failed to capture initial commitment: {err}");
            }
        }
        Persist::<Signer>::persist_new_channel(self.kv.as_ref(), monitor_name, monitor)
    }

    fn update_persisted_channel(
        &self,
        monitor_name: MonitorName,
        monitor_update: Option<&ChannelMonitorUpdate>,
        monitor: &ChannelMonitor<Signer>,
    ) -> ChannelMonitorUpdateStatus {
        if let (Some(ctx), Some(update)) = (self.ctx.get(), monitor_update) {
            let commitment_txs = monitor.counterparty_commitment_txs_from_update(update);
            if let Err(err) = ctx.capture(monitor, commitment_txs) {
                log::error!(target: "lampo-watchtower", "failed to capture justice data: {err}");
            }
        }
        Persist::<Signer>::update_persisted_channel(
            self.kv.as_ref(),
            monitor_name,
            monitor_update,
            monitor,
        )
    }

    fn archive_persisted_channel(&self, monitor_name: MonitorName) {
        Persist::<Signer>::archive_persisted_channel(self.kv.as_ref(), monitor_name)
    }
}
