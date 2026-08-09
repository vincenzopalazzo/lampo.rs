//! The swap engine: two state machines driven by the lampo event
//! stream plus a reconcile tick. Every transition is persisted before
//! it is acted on, so a crash resumes instead of stranding a swap.
//!
//! Direction A (Spark -> LN) is atomic for both sides: the fetched
//! invoice pins the payment hash, the counterparty locks a Spark HTLC
//! on it, and only the preimage revealed by the lightning settlement
//! can claim that HTLC.
//!
//! Direction B (LN -> Spark) has a trusted window in this version:
//! BOLT12 offer payments generate their preimage inside LDK, so the
//! lightning leg settles before the Spark HTLC exists. The payer holds
//! the preimage from settlement; the daemon owes the Spark HTLC and
//! the store guarantees it delivers it across restarts. Closing the
//! window needs an "offer hold" primitive in lampo (see README).
use std::sync::Arc;
use std::time::Duration;

use lampo_common::error;
use lampo_common::event::Event;
use lampo_common::ldk;

use crate::lampo_leg::{hex_encode, offer_id, LampoLeg};
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
    pub amount_msat: u64,
    /// Where the Spark HTLC must be locked.
    pub spark_address: String,
    /// Seconds the quote stays payable. Bounded by LDK reaping the
    /// fetched invoice roughly a minute after the fetch.
    pub expires_in_secs: u64,
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
        let fetched = self.lampo.fetch_invoice(offer, amount_msat).await?;
        // The spark leg settles in sats: reject the swap now rather than
        // after the counterparty has locked funds against the quote.
        if fetched.amount_msat % 1000 != 0 {
            self.lampo.cancel_fetched(&fetched.payment_id).await.ok();
            error::bail!(
                "the invoice asks {}msat, which is not a whole number of sats",
                fetched.amount_msat
            );
        }
        let swap = Swap {
            payment_hash: Some(fetched.payment_hash.clone()),
            offer_id: None,
            direction: Direction::SparkToLn,
            state: State::Quoted,
            amount_msat: fetched.amount_msat,
            lampo_payment_id: Some(fetched.payment_id),
            counterparty_spark_address: None,
            offer: offer.to_owned(),
            created_at: now(),
            updated_at: now(),
        };
        self.store.persist(&swap)?;
        Ok(Quote {
            payment_hash: fetched.payment_hash,
            amount_msat: fetched.amount_msat,
            spark_address: self.spark.spark_address().await?,
            expires_in_secs: self.cfg.quote_expiry_secs,
        })
    }

    /// Direction B entry point: publish an offer whose payments are
    /// forwarded to `spark_address` as Spark HTLCs.
    pub async fn create_receive_offer(
        &self,
        spark_address: &str,
        amount_msat: u64,
    ) -> error::Result<String> {
        if amount_msat % 1000 != 0 {
            error::bail!("{amount_msat}msat is not a whole number of sats");
        }
        let offer = self.lampo.create_offer(Some(amount_msat)).await?;
        let id = offer_id(&offer.bolt12)?;
        let swap = Swap {
            payment_hash: None,
            offer_id: Some(id),
            direction: Direction::LnToSpark,
            state: State::OfferPublished,
            amount_msat,
            lampo_payment_id: None,
            counterparty_spark_address: Some(spark_address.to_owned()),
            offer: offer.bolt12.clone(),
            created_at: now(),
            updated_at: now(),
        };
        self.store.persist(&swap)?;
        Ok(offer.bolt12)
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

    /// Direction B trigger: a payment for one of our offers settled.
    async fn on_lampo_event(&self, event: Event) -> error::Result<()> {
        let Event::RawLDK(ldk::events::Event::PaymentClaimed {
            payment_hash,
            amount_msat,
            purpose:
                ldk::events::PaymentPurpose::Bolt12OfferPayment {
                    payment_context, ..
                },
            ..
        }) = event
        else {
            return Ok(());
        };
        let claimed_offer = hex_encode(payment_context.offer_id.0);
        let Some(mut swap) = self.store.find_by_offer_id(&claimed_offer) else {
            return Ok(());
        };
        if swap.state != State::OfferPublished {
            return Ok(());
        }
        log::info!(
            target: "swapd",
            "offer `{claimed_offer}` paid with `{amount_msat}msat`, hash `{payment_hash}`"
        );
        let old_id = swap.id();
        swap.payment_hash = Some(payment_hash.to_string());
        swap.amount_msat = amount_msat;
        swap.transition(State::LnReceived)?;
        self.store.rekey(&old_id, &swap)?;
        self.deliver_spark_htlc(&mut swap).await
    }

    /// Direction B delivery: lock the Spark HTLC we owe. Also the
    /// crash-recovery path for `LnReceived` swaps found at reconcile.
    async fn deliver_spark_htlc(&self, swap: &mut Swap) -> error::Result<()> {
        let (Some(hash), Some(address)) = (
            swap.payment_hash.clone(),
            swap.counterparty_spark_address.clone(),
        ) else {
            error::bail!("direction B swap `{}` misses hash or address", swap.id());
        };
        // Spark settles in sats. Truncating here would quietly short the
        // counterparty by up to 999 msat per swap, so refuse instead.
        if swap.amount_msat % 1000 != 0 {
            error::bail!(
                "swap `{}` is {}msat, which is not a whole number of sats",
                swap.id(),
                swap.amount_msat
            );
        }
        let amount_sat = swap.amount_msat / 1000;
        match self
            .spark
            .create_htlc(
                amount_sat,
                &address,
                &hash,
                Duration::from_secs(self.cfg.spark_htlc_expiry_secs),
            )
            .await
        {
            Ok(transfer_id) => {
                log::info!(target: "swapd", "spark htlc `{transfer_id}` locked for `{hash}`");
                swap.transition(State::SparkHtlcCreated)?;
                self.store.persist(swap)?;
                // Our obligations are met: the counterparty claims with
                // the preimage their payment revealed, or the HTLC
                // refunds to us at expiry.
                swap.transition(State::Done)?;
                self.store.persist(swap)?;
                Ok(())
            }
            Err(err) => {
                // Leave the swap in LnReceived: reconcile retries the
                // delivery, we still owe it.
                log::error!(target: "swapd", "spark htlc delivery for `{hash}` failed: {err}");
                Err(err)
            }
        }
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
                // The Spark HTLC refunds to the counterparty at its
                // expiry; nothing is owed.
                swap.transition(State::Failed {
                    reason: format!("lightning payment failed: {err}"),
                })?;
                self.store.persist(swap)?;
                return Err(err);
            }
        };
        swap.transition(State::Claiming)?;
        self.store.persist(swap)?;
        self.spark.claim_htlc(&preimage).await?;
        swap.transition(State::Done)?;
        self.store.persist(swap)?;
        log::info!(target: "swapd", "swap `{}` complete", swap.id());
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
                    let locked_sat = claimable
                        .iter()
                        .find(|(locked_hash, _)| locked_hash == &hash)
                        .map(|(_, amount_sat)| *amount_sat);
                    match quoted_action(
                        locked_sat,
                        swap.amount_msat,
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
                    // A restart interrupted `payfetched`. The payment
                    // may have settled, but the preimage lived in the
                    // node's in-memory map: it cannot be recovered over
                    // the API today. Flag loudly instead of guessing.
                    log::error!(
                        target: "swapd",
                        "swap `{}` was paying at restart: the preimage may be lost, manual review needed",
                        swap.id()
                    );
                    swap.transition(State::Failed {
                        reason: "interrupted while paying, needs manual review".to_owned(),
                    })?;
                    self.store.persist(&swap)?;
                }
                (Direction::SparkToLn, State::Claiming) => {
                    // The lightning leg settled but the claim was
                    // interrupted; without the preimage in the store we
                    // cannot retry blindly. Manual review.
                    log::error!(
                        target: "swapd",
                        "swap `{}` was claiming at restart, manual review needed",
                        swap.id()
                    );
                }
                (Direction::LnToSpark, State::LnReceived) => {
                    if let Err(err) = self.deliver_spark_htlc(&mut swap).await {
                        log::error!(target: "swapd", "swap `{}`: {err}", swap.id());
                    }
                }
                (Direction::LnToSpark, State::SparkHtlcCreated) => {
                    swap.transition(State::Done)?;
                    self.store.persist(&swap)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
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
    owed_msat: u64,
    created_at: u64,
    now: u64,
    quote_expiry_secs: u64,
) -> QuotedAction {
    let expected_sat = owed_msat / 1000;
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

    #[test]
    fn a_sufficient_lock_advances() {
        assert_eq!(
            quoted_action(Some(10_000), 10_000_000, 100, 110, 45),
            QuotedAction::Advance
        );
        // overpaying is the counterparty's problem, not a reason to stall
        assert_eq!(
            quoted_action(Some(20_000), 10_000_000, 100, 110, 45),
            QuotedAction::Advance
        );
    }

    #[test]
    fn an_underpaying_lock_is_rejected_not_paid() {
        let action = quoted_action(Some(1), 10_000_000, 100, 110, 45);
        assert!(
            matches!(action, QuotedAction::Reject { .. }),
            "got {action:?}"
        );
    }

    #[test]
    fn an_underpaying_lock_is_rejected_even_after_expiry() {
        // The lock exists, so this must never be treated as an expired
        // quote: the counterparty's funds are in play.
        let action = quoted_action(Some(1), 10_000_000, 100, 100 + 3600, 45);
        assert!(
            matches!(action, QuotedAction::Reject { .. }),
            "got {action:?}"
        );
    }

    #[test]
    fn a_fresh_unlocked_quote_waits() {
        assert_eq!(
            quoted_action(None, 10_000_000, 100, 110, 45),
            QuotedAction::Wait
        );
        // the boundary second still waits, expiry is strictly after
        assert_eq!(
            quoted_action(None, 10_000_000, 100, 145, 45),
            QuotedAction::Wait
        );
    }

    #[test]
    fn an_unlocked_quote_expires_after_the_window() {
        assert_eq!(
            quoted_action(None, 10_000_000, 100, 146, 45),
            QuotedAction::Expire
        );
    }
}
