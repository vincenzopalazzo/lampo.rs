//! The swap engine: two state machines driven by the lampo event
//! stream plus a reconcile tick. Every transition is persisted before
//! it is acted on, so a crash resumes instead of stranding a swap.
//!
//! Direction A (Spark -> LN) is atomic for both sides: the fetched
//! invoice pins the payment hash, the counterparty locks a Spark HTLC
//! on it, and only the preimage revealed by the lightning settlement
//! can claim that HTLC.
//!
//! Direction B (LN -> Spark) is atomic too. The counterparty generates
//! the preimage and gives us only its hash; we issue a hold invoice on
//! that hash, so we cannot settle the lightning leg by ourselves. They
//! pay, the payment is held, we deliver the Spark HTLC on the same
//! hash, and only their claim reveals the preimage that lets us settle.
//! If they never claim, the HTLC refunds to us and the held payment
//! goes back to them.
//!
//! The lightning hold therefore has to outlive the Spark HTLC, because
//! we are the side that settles last. `LN_HOLD_MARGIN_SECS` is that
//! room, and `CLAIM_MARGIN_SECS` is the mirror of it in Direction A,
//! where we are the side that claims last.
use std::sync::Arc;
use std::time::Duration;

use lampo_common::error;
use lampo_common::event::ln::LightningEvent;
use lampo_common::event::Event;

use crate::lampo_leg::LampoLeg;
use crate::settings::Settings;
use crate::spark_leg::SparkLeg;
use crate::store::SwapStore;
use crate::swap::{now, Direction, State, Swap};

pub struct Engine {
    lampo: LampoLeg,
    spark: SparkLeg,
    store: SwapStore,
    cfg: Settings,
}

/// What a Direction A caller needs to lock their side.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Quote {
    pub payment_hash: String,
    /// What the merchant's invoice is for, in msat. Informational.
    pub amount_msat: u64,
    /// What the caller must lock on Spark, in sats: the invoice amount
    /// plus our fee. This is the number that matters to them.
    pub lock_amount_sat: u64,
    /// Our fee, in sats, broken out so the quote is auditable.
    pub fee_sat: u64,
    /// Where the Spark HTLC must be locked.
    pub spark_address: String,
    /// Seconds the quote stays payable. Bounded by LDK reaping the
    /// fetched invoice roughly a minute after the fetch.
    pub expires_in_secs: u64,
    /// The minimum life the locked Spark HTLC must have. Anything
    /// shorter is refused: the lightning payment's CLTV budget must fit
    /// inside the lock, or a slow payment could settle after the lock
    /// refunded and we would pay out with nothing to claim.
    pub min_lock_expiry_secs: u64,
}

impl Engine {
    pub fn new(lampo: LampoLeg, spark: SparkLeg, store: SwapStore, cfg: Settings) -> Self {
        Self {
            lampo,
            spark,
            store,
            cfg,
        }
    }

    /// Direction A entry point: quote paying `offer` from Spark.
    pub async fn quote_spark_to_ln(
        &self,
        offer: &str,
        amount_msat: Option<u64>,
    ) -> error::Result<Quote> {
        let fetched = self
            .lampo
            .fetch_invoice(offer, amount_msat, PAY_CLTV_BUDGET_BLOCKS)
            .await?;
        // The spark leg settles in sats: reject the swap now rather than
        // after the counterparty has locked funds against the quote.
        if fetched.amount_msat % 1000 != 0 {
            self.lampo.cancel_fetched(&fetched.payment_id).await.ok();
            error::bail!(
                "the invoice asks {}msat, which is not a whole number of sats",
                fetched.amount_msat
            );
        }
        // The lightning leg is the invoice; the spark leg is that plus
        // our fee. The counterparty locks the larger amount, we claim
        // it, and we pay the smaller invoice: the gap is what covers
        // routing and pays us.
        let invoice_sat = fetched.amount_msat / 1000;
        if invoice_sat > self.cfg.max_swap_sat {
            self.lampo.cancel_fetched(&fetched.payment_id).await.ok();
            error::bail!(
                "the invoice asks {invoice_sat} sat, above the {} sat swap limit",
                self.cfg.max_swap_sat
            );
        }
        // The fee has to cover the lightning routing *we* pay on this
        // leg. LDK caps BOLT12 routing at 1% + 50 sat by default, so if
        // we charged less we could route-pay more than we collect and
        // settle the swap at a loss. Floor the fee at that cap.
        let fee_sat = self.fee_sat(invoice_sat).max(max_routing_sat(invoice_sat));
        let lock_amount_sat = invoice_sat + fee_sat;
        let swap = Swap {
            payment_hash: Some(fetched.payment_hash.clone()),
            offer_id: None,
            direction: Direction::SparkToLn,
            state: State::Quoted,
            amount_msat: fetched.amount_msat,
            spark_amount_sat: lock_amount_sat,
            lampo_payment_id: Some(fetched.payment_id),
            spark_transfer_id: None,
            preimage: None,
            counterparty_spark_address: None,
            spark_locked_at: None,
            offer: offer.to_owned(),
            created_at: now(),
            updated_at: now(),
        };
        self.store.persist(&swap)?;
        Ok(Quote {
            payment_hash: fetched.payment_hash,
            amount_msat: fetched.amount_msat,
            lock_amount_sat,
            fee_sat,
            spark_address: self.spark.spark_address().await?,
            expires_in_secs: self.cfg.quote_expiry_secs,
            min_lock_expiry_secs: required_lock_secs(),
        })
    }

    /// Our fee for a swap of `amount_sat`: a flat base plus a
    /// proportional part. Covers lightning routing, which we absorb, and
    /// the capital and timing risk of fronting both legs.
    fn fee_sat(&self, amount_sat: u64) -> u64 {
        self.cfg.fee_base_sat + amount_sat * self.cfg.fee_ppm / 1_000_000
    }

    /// Direction B entry point: the counterparty gives us the payment
    /// hash of a preimage *they* generated, and we issue a hold invoice
    /// on it.
    ///
    /// Taking the hash rather than minting our own is what makes this
    /// direction atomic. We physically cannot settle the lightning leg
    /// until they claim their Spark HTLC and reveal the secret, so the
    /// old window where we were paid but had not yet delivered is gone.
    pub async fn create_hold_swap(
        &self,
        spark_address: &str,
        payout_sat: u64,
        payment_hash: &str,
    ) -> error::Result<String> {
        if hex_bytes(payment_hash).map(|bytes| bytes.len()) != Some(32) {
            error::bail!("`payment_hash` must be 32 bytes of hex");
        }
        if payout_sat > self.cfg.max_swap_sat {
            // Every accepted swap locks our own funds in an htlc for its
            // whole expiry, at no cost to a counterparty who never
            // claims. The cap bounds that exposure.
            error::bail!(
                "a {payout_sat} sat payout is above the {} sat swap limit",
                self.cfg.max_swap_sat
            );
        }
        if self.store.get(payment_hash).is_some() {
            // A hash may back exactly one swap, ever. Reusing one would
            // let a second payment ride on the first swap's htlc.
            error::bail!("a swap for this payment hash already exists");
        }

        // They ask to receive `payout_sat` on spark; the invoice they
        // pay is that plus our fee. We receive the larger amount on
        // lightning and deliver the smaller on spark.
        let fee_sat = self.fee_sat(payout_sat);
        let invoice_msat = (payout_sat + fee_sat) * 1000;

        // The lightning hold must outlive the spark htlc: we can only
        // settle it after they claim, and they can only claim while the
        // spark leg is alive. `LN_HOLD_MARGIN` is the room this leaves.
        let hold_secs = self.cfg.spark_htlc_expiry_secs + LN_HOLD_MARGIN_SECS;
        let cltv = cltv_blocks_for(hold_secs)?;

        let invoice = self
            .lampo
            .hold_invoice(payment_hash, invoice_msat, cltv, hold_secs as u32)
            .await?;
        let swap = Swap {
            payment_hash: Some(payment_hash.to_owned()),
            offer_id: None,
            direction: Direction::LnToSpark,
            state: State::HoldInvoiceIssued,
            amount_msat: invoice_msat,
            spark_amount_sat: payout_sat,
            lampo_payment_id: None,
            // Chosen now, before anything is sent, so a retry after a
            // crash reuses it instead of delivering twice.
            spark_transfer_id: Some(new_transfer_id()),
            preimage: None,
            counterparty_spark_address: Some(spark_address.to_owned()),
            spark_locked_at: None,
            offer: invoice.bolt11.clone(),
            created_at: now(),
            updated_at: now(),
        };
        self.store.persist(&swap)?;
        Ok(invoice.bolt11)
    }

    pub fn list(&self) -> Vec<Swap> {
        self.store.list()
    }

    /// Run the engine until the process shuts down: consume lampo
    /// events and reconcile pending swaps on a tick.
    pub async fn run(self: Arc<Self>) {
        let mut events = self.lampo.events();
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                event = events.recv() => {
                    let Some(event) = event else {
                        log::error!(target: "swapd", "lampo event stream closed, stopping the engine");
                        return;
                    };
                    if let Err(err) = self.on_lampo_event(event).await {
                        log::error!(target: "swapd", "event handling failed: {err}");
                    }
                }
                _ = tick.tick() => {
                    if let Err(err) = self.reconcile().await {
                        log::error!(target: "swapd", "reconcile failed: {err}");
                    }
                }
            }
        }
    }

    /// Direction B trigger: their payment arrived and is *held*. We
    /// have not been paid yet, and cannot be until they claim.
    async fn on_lampo_event(&self, event: Event) -> error::Result<()> {
        let Event::Lightning(LightningEvent::PaymentHeld {
            payment_hash,
            amount_msat,
            claim_deadline,
        }) = event
        else {
            return Ok(());
        };
        let Some(mut swap) = self.store.get(&payment_hash) else {
            return Ok(());
        };
        if swap.state != State::HoldInvoiceIssued {
            return Ok(());
        }
        log::info!(
            target: "swapd",
            "payment `{payment_hash}` held for `{amount_msat}msat` (deadline `{claim_deadline:?}`)"
        );

        if let Err(reason) = check_received_amount(amount_msat, swap.amount_msat) {
            log::error!(target: "swapd", "swap `{}`: {reason}", swap.id());
            // Give it straight back: we never took it.
            self.lampo.hold_fail(&payment_hash).await.ok();
            swap.transition(State::Failed { reason })?;
            self.store.persist(&swap)?;
            return Ok(());
        }

        swap.transition(State::LnHeld)?;
        self.store.persist(&swap)?;
        self.deliver_spark_htlc(&mut swap).await
    }

    /// Direction B delivery: lock the Spark HTLC we owe them. Also the
    /// crash-recovery path for `LnHeld` swaps found at reconcile.
    ///
    /// The transfer id was chosen and persisted before we ever called
    /// Spark, so a retry here reuses it and Spark refuses the duplicate
    /// rather than sending a second transfer.
    async fn deliver_spark_htlc(&self, swap: &mut Swap) -> error::Result<()> {
        let (Some(hash), Some(address), Some(transfer_id)) = (
            swap.payment_hash.clone(),
            swap.counterparty_spark_address.clone(),
            swap.spark_transfer_id.clone(),
        ) else {
            error::bail!("direction B swap `{}` is missing its fields", swap.id());
        };
        let amount_sat = swap.spark_amount_sat;
        // Try the payout as-is first: if a leaf already covers the
        // amount (the common case, and always true when the amount
        // matches a deposit), this just works. Only if leaf selection
        // fails do we reshape into spendable denominations and retry,
        // because the optimizer reserves leaves and can leave the wallet
        // briefly unspendable if it stumbles.
        let mut result = self
            .spark
            .create_htlc(
                amount_sat,
                &address,
                &hash,
                Duration::from_secs(self.cfg.spark_htlc_expiry_secs),
                &transfer_id,
            )
            .await;
        if result.is_err() {
            log::info!(target: "swapd", "reshaping leaves to cover {amount_sat} sat");
            self.spark.optimize(LEAF_OPTIMIZE_ROUNDS).await.ok();
            result = self
                .spark
                .create_htlc(
                    amount_sat,
                    &address,
                    &hash,
                    Duration::from_secs(self.cfg.spark_htlc_expiry_secs),
                    &transfer_id,
                )
                .await;
        }
        match result {
            Ok(id) => {
                log::info!(target: "swapd", "spark htlc `{id}` locked for `{hash}`");
                // Stamp when the htlc was actually locked: its real
                // expiry is measured from now, and returning the held
                // lightning payment before that expiry (plus a margin)
                // would let them claim spark *and* keep their refund.
                swap.spark_locked_at = Some(now());
                swap.transition(State::SparkHtlcLocked)?;
                self.store.persist(swap)?;
                Ok(())
            }
            Err(err) => {
                // Stay in LnHeld: we still owe it and reconcile retries.
                // Their payment is still held, so nobody has lost value.
                log::error!(target: "swapd", "spark htlc delivery for `{hash}` failed: {err}");
                Err(err)
            }
        }
    }

    /// Direction B settlement: they claimed the Spark HTLC, which
    /// revealed the preimage; that is the only thing that lets us take
    /// the held lightning payment.
    /// Returns `true` if it settled (the preimage was revealed and the
    /// held payment claimed), `false` if there is still no preimage.
    async fn settle_from_revealed_preimage(&self, swap: &mut Swap) -> error::Result<bool> {
        let (Some(hash), Some(transfer_id)) =
            (swap.payment_hash.clone(), swap.spark_transfer_id.clone())
        else {
            error::bail!("direction B swap `{}` is missing its fields", swap.id());
        };
        let Some(preimage) = self.spark.revealed_preimage(&transfer_id).await? else {
            return Ok(false);
        };
        log::info!(target: "swapd", "swap `{hash}` preimage revealed, settling the held payment");
        self.lampo.hold_claim(&preimage).await?;
        swap.transition(State::Done)?;
        self.store.persist(swap)?;
        Ok(true)
    }

    /// Direction A advance: the counterparty locked their Spark HTLC,
    /// pay the lightning leg and claim with the revealed preimage.
    async fn advance_spark_to_ln(&self, swap: &mut Swap) -> error::Result<()> {
        let Some(payment_id) = swap.lampo_payment_id.clone() else {
            error::bail!("direction A swap `{}` misses the payment id", swap.id());
        };
        swap.transition(State::LnPaying)?;
        self.store.persist(swap)?;
        let pay = self.lampo.pay_fetched(&payment_id).await;
        let preimage = match pay {
            Ok(pay) if pay.payment_preimage.is_some() => {
                // SAFETY: checked just above.
                pay.payment_preimage.unwrap()
            }
            Ok(pay) => {
                let reason = format!(
                    "lightning payment ended without a preimage: {:?}",
                    pay.state
                );
                swap.transition(State::Failed {
                    reason: reason.clone(),
                })?;
                self.store.persist(swap)?;
                error::bail!("{reason}");
            }
            Err(err) => {
                // An error here does NOT mean the payment failed: the
                // node's wait has a timeout and its htlcs may still be
                // in flight. Marking the swap failed now would strand a
                // late settlement -- paid out, never claimed. Stay in
                // `LnPaying`; reconcile resolves it from evidence: the
                // preimage appears (claim) or the htlc dies without one
                // (close). A payment that truly failed simply never
                // produces a preimage and the record closes at expiry.
                log::warn!(
                    target: "swapd",
                    "swap `{}` payment outcome unknown ({err}), reconcile will resolve it",
                    swap.id()
                );
                return Err(err);
            }
        };
        // Persist the preimage *before* claiming, so a crash between the
        // lightning settling and the spark claim can retry instead of
        // losing the secret. This is what makes `Claiming` recoverable.
        swap.preimage = Some(preimage.clone());
        swap.transition(State::Claiming)?;
        self.store.persist(swap)?;
        self.spark.claim_htlc(&preimage).await?;
        swap.transition(State::Done)?;
        self.store.persist(swap)?;
        log::info!(target: "swapd", "swap `{}` complete", swap.id());
        Ok(())
    }

    /// Recover a Direction A swap that crashed while paying. The node
    /// persisted the preimage on settlement, so ask it: settled means we
    /// can claim. Not settled is ambiguous — the payment may have failed
    /// *or still be in flight* — so while the counterparty's htlc is
    /// still claimable we wait and ask again next tick. Declaring the
    /// payment dead while it can still settle would strand a later
    /// settlement: paid out, nothing claimed. Only once the htlc is gone
    /// (nothing left to claim either way) do we close the record.
    async fn recover_ln_paying(
        &self,
        swap: &mut Swap,
        htlc_still_claimable: bool,
    ) -> error::Result<()> {
        let hash = swap.payment_hash.clone().unwrap_or_default();
        match self.lampo.payment_preimage(&hash).await? {
            Some(preimage) => {
                log::info!(target: "swapd", "swap `{hash}` settled while down, resuming the claim");
                swap.preimage = Some(preimage);
                swap.transition(State::Claiming)?;
                self.store.persist(swap)?;
                self.retry_claim(swap).await
            }
            None if htlc_still_claimable => {
                // The payment's cltv budget fits inside the htlc's life,
                // so if it ever settles it does so while we can still
                // claim. Keep waiting; the next tick asks again.
                log::info!(
                    target: "swapd",
                    "swap `{hash}` not settled yet, waiting while the spark htlc is alive"
                );
                Ok(())
            }
            None => {
                log::warn!(
                    target: "swapd",
                    "swap `{hash}` was not settled and the spark htlc is gone, closing it"
                );
                swap.transition(State::Failed {
                    reason: "interrupted while paying; the payment did not settle".to_owned(),
                })?;
                self.store.persist(swap)?;
                Ok(())
            }
        }
    }

    /// Claim the counterparty's Spark HTLC with the preimage we persisted
    /// before the claim. Idempotent: an already-claimed htlc is a
    /// harmless no-op, so retrying after a crash is safe.
    async fn retry_claim(&self, swap: &mut Swap) -> error::Result<()> {
        let Some(preimage) = swap.preimage.clone() else {
            error::bail!("swap `{}` is claiming without a stored preimage", swap.id());
        };
        self.spark.claim_htlc(&preimage).await?;
        swap.transition(State::Done)?;
        self.store.persist(swap)?;
        log::info!(target: "swapd", "swap `{}` complete after recovery", swap.id());
        Ok(())
    }

    /// Periodic reconciliation: expire stale quotes, detect locked
    /// Spark HTLCs, and finish work interrupted by a restart.
    async fn reconcile(&self) -> error::Result<()> {
        let pending = self.store.pending();
        if pending.is_empty() {
            return Ok(());
        }
        let claimable = self.spark.claimable_htlcs().await.unwrap_or_default();
        for mut swap in pending {
            match (&swap.direction, swap.state.clone()) {
                (Direction::SparkToLn, State::Quoted) => {
                    let hash = swap.payment_hash.clone().unwrap_or_default();
                    let locked = claimable.iter().find(|htlc| htlc.payment_hash == hash);
                    // Their expiry is their choice. It must outlive the
                    // payment's whole CLTV budget plus the claim margin:
                    // a payment can stay in flight for its full budget,
                    // and if the lock refunds first, a late settlement
                    // pays the merchant with nothing left to claim.
                    if let Some(htlc) = locked {
                        if !expiry_leaves_room(htlc.expiry, required_lock_secs()) {
                            log::error!(
                                target: "swapd",
                                "swap `{}` locked with too little time left to claim safely",
                                swap.id()
                            );
                            swap.transition(State::Failed {
                                reason: "the spark htlc expires too soon to claim safely"
                                    .to_owned(),
                            })?;
                            self.store.persist(&swap)?;
                            continue;
                        }
                    }
                    let locked_sat = locked.map(|htlc| htlc.amount_sat);
                    match quoted_action(
                        locked_sat,
                        swap.spark_amount_sat,
                        swap.created_at,
                        now(),
                        self.cfg.quote_expiry_secs,
                    ) {
                        QuotedAction::Wait => {}
                        QuotedAction::Advance => {
                            if let Err(err) = self.advance_spark_to_ln(&mut swap).await {
                                log::error!(target: "swapd", "swap `{}`: {err}", swap.id());
                            }
                        }
                        QuotedAction::Reject { reason } => {
                            log::error!(target: "swapd", "swap `{}`: {reason}", swap.id());
                            swap.transition(State::Failed { reason })?;
                            self.store.persist(&swap)?;
                        }
                        QuotedAction::Expire => {
                            if let Some(payment_id) = swap.lampo_payment_id.clone() {
                                let _ = self.lampo.cancel_fetched(&payment_id).await;
                            }
                            swap.transition(State::Failed {
                                reason: "quote expired before the spark htlc was locked".to_owned(),
                            })?;
                            self.store.persist(&swap)?;
                        }
                    }
                }
                (Direction::SparkToLn, State::LnPaying) => {
                    // A restart interrupted `payfetched`. Ask the node
                    // whether the payment settled: it kept the preimage
                    // even though we lost it. Settled means claim; not
                    // settled means wait while the htlc lives (the
                    // payment may still be in flight) and close only
                    // once it is gone.
                    let hash = swap.payment_hash.clone().unwrap_or_default();
                    let htlc_alive = claimable
                        .iter()
                        .any(|htlc| htlc.payment_hash == hash && expiry_leaves_room(htlc.expiry, 0));
                    if let Err(err) = self.recover_ln_paying(&mut swap, htlc_alive).await {
                        log::error!(target: "swapd", "swap `{}`: {err}", swap.id());
                    }
                }
                (Direction::SparkToLn, State::Claiming) => {
                    // The lightning leg settled and we persisted the
                    // preimage before crashing, so just retry the claim.
                    if let Err(err) = self.retry_claim(&mut swap).await {
                        log::error!(target: "swapd", "swap `{}`: {err}", swap.id());
                    }
                }
                (Direction::LnToSpark, State::LnHeld) => {
                    // We owe the htlc. Retrying is safe: the transfer id
                    // is fixed, so a duplicate is refused rather than sent.
                    if let Err(err) = self.deliver_spark_htlc(&mut swap).await {
                        log::error!(target: "swapd", "swap `{}`: {err}", swap.id());
                    }
                }
                (Direction::LnToSpark, State::SparkHtlcLocked) => {
                    // Waiting for them to claim. When they do, the
                    // preimage appears and settles our side. If they
                    // never claim, the spark htlc refunds to us and the
                    // held payment goes back to them: both or neither.
                    match self.settle_from_revealed_preimage(&mut swap).await {
                        Ok(true) => {}
                        Ok(false) => {
                            // Not claimed yet. We may only return the held
                            // lightning payment once the spark htlc is
                            // provably dead: past its real expiry, which is
                            // measured from when we *locked* it (not from
                            // `created_at`, which precedes the lock), plus a
                            // safety margin. Returning it any earlier lets
                            // the counterparty claim the still-live spark
                            // htlc *after* we have refunded them on
                            // lightning -- a total loss of the payout. The
                            // lightning hold outlives the spark htlc by
                            // LN_HOLD_MARGIN_SECS, so there is room to wait.
                            let locked_at = swap.spark_locked_at.unwrap_or(swap.updated_at);
                            if held_payment_returnable(
                                locked_at,
                                self.cfg.spark_htlc_expiry_secs,
                                now(),
                            ) {
                                log::warn!(
                                    target: "swapd",
                                    "swap `{}` expired unclaimed, returning the held payment",
                                    swap.id()
                                );
                                if let Some(hash) = swap.payment_hash.clone() {
                                    self.lampo.hold_fail(&hash).await.ok();
                                }
                                swap.transition(State::Failed {
                                    reason: "counterparty never claimed the spark htlc".to_owned(),
                                })?;
                                self.store.persist(&swap)?;
                            }
                        }
                        Err(err) => {
                            log::error!(target: "swapd", "swap `{}`: {err}", swap.id());
                        }
                    }
                }
                (Direction::LnToSpark, State::HoldInvoiceIssued) => {
                    // Nobody paid yet. Nothing is at risk, so just let
                    // the invoice expire on the node.
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// How many rounds of leaf optimization to run before a payout, enough
/// to split a single deposit leaf into spendable denominations.
const LEAF_OPTIMIZE_ROUNDS: u32 = 3;

/// The most lightning routing we might pay on a Direction A leg, in sats.
/// LDK's default BOLT12 route budget is 1% of the amount plus 50 sat
/// (`RouteParameters::from_payment_params_and_value`); our fee must never
/// dip below this or the swap can settle for a loss.
fn max_routing_sat(amount_sat: u64) -> u64 {
    amount_sat / 100 + 50
}

/// How much longer the held lightning payment must live than the spark
/// htlc it is paired with. We can only settle lightning *after* they
/// claim spark, so the lightning side has to be the last to expire.
const LN_HOLD_MARGIN_SECS: u64 = 3600;

/// How long past a spark htlc's expiry we wait before returning the
/// paired held lightning payment, so the htlc is provably refunded to us
/// and can no longer be claimed. Must be well under `LN_HOLD_MARGIN_SECS`
/// so we still return the payment before the lightning hold itself times
/// out on the node.
const SETTLE_SAFETY_MARGIN_SECS: u64 = 600;

/// How much of a spark htlc's remaining life we insist on before paying
/// the other leg, so learning the preimage still leaves time to claim.
const CLAIM_MARGIN_SECS: u64 = 600;

/// The total CLTV budget we give a Direction A lightning payment, in
/// blocks. This bounds how long the payment can stay in flight: it must
/// settle or fail within this many blocks. Passed to the node at fetch
/// time, and the counterparty's spark htlc must outlive it (plus the
/// claim margin), or a payment stuck for its full CLTV could settle
/// *after* their htlc refunded to them, leaving us paid-out with
/// nothing to claim.
///
/// 432 blocks is three days. It cannot be much tighter: a BOLT12
/// blinded path alone aggregates the final delta, the intro hop's
/// delta (72 on a plain LDK channel) and padding — 144 already fails
/// with `RouteNotFound` on a *direct* channel — while LDK's default
/// budget of 1008 (a week) would demand a spark lock nobody wants.
const PAY_CLTV_BUDGET_BLOCKS: u32 = 432;

/// The spark htlc life a Direction A counterparty must lock for: the
/// payment's worst-case in-flight time plus the room to claim after
/// the preimage is revealed.
const fn required_lock_secs() -> u64 {
    PAY_CLTV_BUDGET_BLOCKS as u64 * SECS_PER_BLOCK + CLAIM_MARGIN_SECS
}

/// Lightning measures time in blocks; spark in seconds. Ten minutes a
/// block is the usual approximation, and rounding up is the safe
/// direction because it only ever asks for more room.
const SECS_PER_BLOCK: u64 = 600;

/// Overpayment we keep without complaint, mirroring the shape Boltz
/// settled on: a flat floor for dust, or a small percentage, whichever
/// is larger.
const OVERPAY_EXEMPT_MSAT: u64 = 10_000_000;
const OVERPAY_PERCENT: u64 = 2;
/// Anything past this multiple is refused outright rather than pocketed:
/// a payment many times the quote is a bug or an attack, not a tip.
const OVERPAY_HARD_CAP_MULTIPLE: u64 = 2;

/// Convert a hold duration into the final cltv delta the invoice needs.
fn cltv_blocks_for(hold_secs: u64) -> error::Result<u16> {
    let blocks = hold_secs.div_ceil(SECS_PER_BLOCK);
    // LDK fails held htlcs back ~39 blocks before their expiry, so the
    // delta has to cover the hold *and* that buffer.
    let blocks = blocks + 40;
    u16::try_from(blocks)
        .map_err(|_| error::anyhow!("a {hold_secs}s hold needs {blocks} blocks, too many"))
}

/// May we return a Direction B held lightning payment yet? Only once the
/// paired spark htlc is provably dead: past its expiry, measured from
/// when we locked it, plus a safety margin so it has refunded to us and
/// can no longer be claimed. Returning it earlier is a fund drain -- the
/// counterparty could claim the live spark htlc after we refund them.
fn held_payment_returnable(spark_locked_at: u64, spark_expiry_secs: u64, now: u64) -> bool {
    now > spark_locked_at + spark_expiry_secs + SETTLE_SAFETY_MARGIN_SECS
}

/// Does `expiry` leave at least `margin` from now?
fn expiry_leaves_room(expiry: std::time::SystemTime, margin: u64) -> bool {
    expiry
        .duration_since(std::time::SystemTime::now())
        .map(|left| left.as_secs() >= margin)
        .unwrap_or(false)
}

/// Accept the payment, or say why not. Underpayment is always refused;
/// overpayment is kept up to a bound and refused past a hard multiple.
fn check_received_amount(received_msat: u64, agreed_msat: u64) -> Result<(), String> {
    if received_msat < agreed_msat {
        return Err(format!(
            "lightning paid {received_msat} msat, less than the agreed {agreed_msat}"
        ));
    }
    if received_msat > agreed_msat.saturating_mul(OVERPAY_HARD_CAP_MULTIPLE) {
        return Err(format!(
            "lightning paid {received_msat} msat, more than {OVERPAY_HARD_CAP_MULTIPLE}x the agreed {agreed_msat}"
        ));
    }
    let slippage = OVERPAY_EXEMPT_MSAT.max(agreed_msat * OVERPAY_PERCENT / 100);
    if received_msat - agreed_msat > slippage {
        return Err(format!(
            "lightning overpaid by {} msat, beyond the {slippage} msat allowed",
            received_msat - agreed_msat
        ));
    }
    Ok(())
}

fn hex_bytes(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// A fresh uuid-shaped transfer id, chosen before any transfer is sent.
fn new_transfer_id() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_err()
    {
        // Not security critical: this is a uniqueness key, and the
        // store rejects a collision anyway.
        let nanos = now();
        bytes[..8].copy_from_slice(&nanos.to_le_bytes());
    }
    // uuid v4 shape, which is what Spark's TransferId parses.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// What reconcile does with a Direction A swap still in `Quoted`.
#[derive(Debug, PartialEq, Eq)]
enum QuotedAction {
    /// Nothing locked yet and the quote is still fresh.
    Wait,
    /// A sufficient htlc is locked: pay the lightning leg.
    Advance,
    /// An htlc is locked but it does not cover the swap. Anyone can
    /// lock against a quoted hash, so paying here would settle an
    /// expensive invoice and reveal the preimage for less than it
    /// bought: the swap must fail instead.
    Reject { reason: String },
    /// Nothing was locked inside the quote window.
    Expire,
}

fn quoted_action(
    locked_sat: Option<u64>,
    expected_sat: u64,
    created_at: u64,
    now: u64,
    quote_expiry_secs: u64,
) -> QuotedAction {
    match locked_sat {
        Some(locked_sat) if locked_sat < expected_sat => QuotedAction::Reject {
            reason: format!("spark htlc locks {locked_sat} sat, the swap needs {expected_sat} sat"),
        },
        Some(_) => QuotedAction::Advance,
        None if now > created_at + quote_expiry_secs => QuotedAction::Expire,
        None => QuotedAction::Wait,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings(base: u64, ppm: u64) -> Settings {
        Settings {
            spark_network: "regtest".to_owned(),
            spark_seed_file: std::path::PathBuf::from("/dev/null"),
            quote_expiry_secs: 45,
            spark_htlc_expiry_secs: 3600,
            api_addr: "127.0.0.1:0".to_owned(),
            spark_operators: Vec::new(),
            fee_base_sat: base,
            fee_ppm: ppm,
            max_swap_sat: 1_000_000,
        }
    }

    fn fee_of(base: u64, ppm: u64, amount: u64) -> u64 {
        // mirror Engine::fee_sat without constructing the legs
        let cfg = test_settings(base, ppm);
        cfg.fee_base_sat + amount * cfg.fee_ppm / 1_000_000
    }

    #[test]
    fn the_fee_is_base_plus_proportional() {
        // 1 sat flat + 0.5% of 50_000 = 1 + 250
        assert_eq!(fee_of(1, 5_000, 50_000), 251);
        // pure base
        assert_eq!(fee_of(10, 0, 50_000), 10);
        // pure proportional, 1%
        assert_eq!(fee_of(0, 10_000, 1_000_000), 10_000);
    }

    #[test]
    fn the_fee_never_dips_below_the_routing_we_might_pay() {
        // Direction A pays lightning routing out of the fee. LDK's
        // default budget is 1% + 50 sat; the effective fee (config vs
        // that floor, whichever is larger) must cover it, or the swap
        // settles for a loss.
        let invoice_sat = 1_000_000u64;
        let configured = fee_of(1, 5_000, invoice_sat); // 0.5% + 1 = 5_001
        let floor = max_routing_sat(invoice_sat); // 1% + 50 = 10_050
        let effective = configured.max(floor);
        assert_eq!(effective, floor, "the routing floor must win when config is thinner");
        assert!(effective >= max_routing_sat(invoice_sat));
    }

    #[test]
    fn underpayment_is_refused() {
        assert!(check_received_amount(999, 1_000).is_err());
    }

    #[test]
    fn exact_and_small_overpayment_are_accepted() {
        assert!(check_received_amount(50_000_000, 50_000_000).is_ok());
        // the 1 sat lightning routinely adds
        assert!(check_received_amount(50_001_000, 50_000_000).is_ok());
    }

    #[test]
    fn absurd_overpayment_is_refused_not_pocketed() {
        // past the hard multiple: a bug or an attack, not a tip
        assert!(check_received_amount(50_000_000 * 3, 50_000_000).is_err());
    }

    #[test]
    fn overpayment_past_the_slippage_bound_is_refused() {
        // within 2x but far beyond both the flat floor and the percentage
        let agreed = 1_000_000_000;
        let slippage = OVERPAY_EXEMPT_MSAT.max(agreed * OVERPAY_PERCENT / 100);
        assert!(check_received_amount(agreed + slippage, agreed).is_ok());
        assert!(check_received_amount(agreed + slippage + 1, agreed).is_err());
    }

    #[test]
    fn the_lightning_hold_outlives_the_spark_leg() {
        // The whole safety property of direction B in one assertion: the
        // cltv we ask for must cover the spark expiry plus the margin.
        let spark_secs = 3600;
        let hold_secs = spark_secs + LN_HOLD_MARGIN_SECS;
        let blocks = cltv_blocks_for(hold_secs).unwrap();
        assert!(
            (blocks as u64) * SECS_PER_BLOCK > spark_secs + LN_HOLD_MARGIN_SECS,
            "cltv {blocks} blocks must outlast the spark leg"
        );
    }

    #[test]
    fn the_held_payment_is_not_returned_before_the_spark_htlc_is_dead() {
        let locked_at = 1_000;
        let expiry = 3_600;
        // While the htlc is still live, the held payment must NOT be
        // returned: returning it lets the counterparty claim spark and
        // keep their lightning refund. This is the drain the anchor bug
        // caused when it measured from `created_at` instead of lock time.
        assert!(!held_payment_returnable(locked_at, expiry, locked_at));
        assert!(!held_payment_returnable(locked_at, expiry, locked_at + expiry));
        // At expiry it is still not safe: no margin for the refund to
        // settle and for clock skew.
        assert!(!held_payment_returnable(
            locked_at,
            expiry,
            locked_at + expiry + SETTLE_SAFETY_MARGIN_SECS
        ));
        // Only once the htlc is provably dead, expiry + margin past the
        // lock, may we return it.
        assert!(held_payment_returnable(
            locked_at,
            expiry,
            locked_at + expiry + SETTLE_SAFETY_MARGIN_SECS + 1
        ));
    }

    #[test]
    fn the_required_lock_outlives_the_payment_cltv_budget() {
        // A Direction A payment can stay in flight for its whole cltv
        // budget. The lock we demand from the counterparty must cover
        // that entire window plus the room to claim afterwards --
        // anything less and a stuck payment can settle after their
        // htlc refunded, paying out with nothing left to claim.
        assert!(
            required_lock_secs() >= PAY_CLTV_BUDGET_BLOCKS as u64 * SECS_PER_BLOCK + CLAIM_MARGIN_SECS
        );
        // And the budget itself must leave room for a realistic route:
        // a final delta of ~72 plus a few hops.
        assert!(PAY_CLTV_BUDGET_BLOCKS >= 100);
    }

    #[test]
    fn the_return_margin_stays_within_the_lightning_hold() {
        // We must return the held payment before the lightning hold
        // itself expires on the node, or LDK fails it back for us and
        // the margin bought nothing. The hold outlives the spark htlc by
        // LN_HOLD_MARGIN_SECS, and we act SETTLE_SAFETY_MARGIN_SECS after
        // the spark expiry, so the safety margin must be the smaller.
        assert!(SETTLE_SAFETY_MARGIN_SECS < LN_HOLD_MARGIN_SECS);
    }

    #[test]
    fn an_absurd_hold_is_refused_rather_than_truncated() {
        assert!(cltv_blocks_for(u64::MAX / 2).is_err());
    }

    #[test]
    fn expiry_room_is_measured_against_the_margin() {
        let soon = std::time::SystemTime::now() + Duration::from_secs(60);
        let later = std::time::SystemTime::now() + Duration::from_secs(CLAIM_MARGIN_SECS + 60);
        assert!(!expiry_leaves_room(soon, CLAIM_MARGIN_SECS));
        assert!(expiry_leaves_room(later, CLAIM_MARGIN_SECS));
        // already expired
        assert!(!expiry_leaves_room(
            std::time::SystemTime::now() - Duration::from_secs(1),
            CLAIM_MARGIN_SECS
        ));
    }

    #[test]
    fn transfer_ids_are_unique_and_uuid_shaped() {
        let a = new_transfer_id();
        let b = new_transfer_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.matches('-').count(), 4);
    }

    #[test]
    fn a_sufficient_lock_advances() {
        assert_eq!(
            quoted_action(Some(10_000), 10_000, 100, 110, 45),
            QuotedAction::Advance
        );
        // overpaying is the counterparty's problem, not a reason to stall
        assert_eq!(
            quoted_action(Some(20_000), 10_000, 100, 110, 45),
            QuotedAction::Advance
        );
    }

    #[test]
    fn an_underpaying_lock_is_rejected_not_paid() {
        let action = quoted_action(Some(1), 10_000, 100, 110, 45);
        assert!(
            matches!(action, QuotedAction::Reject { .. }),
            "got {action:?}"
        );
    }

    #[test]
    fn an_underpaying_lock_is_rejected_even_after_expiry() {
        // The lock exists, so this must never be treated as an expired
        // quote: the counterparty's funds are in play.
        let action = quoted_action(Some(1), 10_000, 100, 100 + 3600, 45);
        assert!(
            matches!(action, QuotedAction::Reject { .. }),
            "got {action:?}"
        );
    }

    #[test]
    fn a_fresh_unlocked_quote_waits() {
        assert_eq!(
            quoted_action(None, 10_000, 100, 110, 45),
            QuotedAction::Wait
        );
        // the boundary second still waits, expiry is strictly after
        assert_eq!(
            quoted_action(None, 10_000, 100, 145, 45),
            QuotedAction::Wait
        );
    }

    #[test]
    fn an_unlocked_quote_expires_after_the_window() {
        assert_eq!(
            quoted_action(None, 10_000, 100, 146, 45),
            QuotedAction::Expire
        );
    }
}
