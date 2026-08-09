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
            spark_transfer_id: None,
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
        amount_msat: u64,
        payment_hash: &str,
    ) -> error::Result<String> {
        if amount_msat % 1000 != 0 {
            error::bail!("{amount_msat}msat is not a whole number of sats");
        }
        if hex_bytes(payment_hash).map(|bytes| bytes.len()) != Some(32) {
            error::bail!("`payment_hash` must be 32 bytes of hex");
        }
        if self.store.get(payment_hash).is_some() {
            // A hash may back exactly one swap, ever. Reusing one would
            // let a second payment ride on the first swap's htlc.
            error::bail!("a swap for this payment hash already exists");
        }

        // The lightning hold must outlive the spark htlc: we can only
        // settle it after they claim, and they can only claim while the
        // spark leg is alive. `LN_HOLD_MARGIN` is the room this leaves.
        let hold_secs = self.cfg.spark_htlc_expiry_secs + LN_HOLD_MARGIN_SECS;
        let cltv = cltv_blocks_for(hold_secs)?;

        let invoice = self
            .lampo
            .hold_invoice(payment_hash, amount_msat, cltv, hold_secs as u32)
            .await?;
        let swap = Swap {
            payment_hash: Some(payment_hash.to_owned()),
            offer_id: None,
            direction: Direction::LnToSpark,
            state: State::HoldInvoiceIssued,
            amount_msat,
            lampo_payment_id: None,
            // Chosen now, before anything is sent, so a retry after a
            // crash reuses it instead of delivering twice.
            spark_transfer_id: Some(new_transfer_id()),
            counterparty_spark_address: Some(spark_address.to_owned()),
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
                &transfer_id,
            )
            .await
        {
            Ok(id) => {
                log::info!(target: "swapd", "spark htlc `{id}` locked for `{hash}`");
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
    async fn settle_from_revealed_preimage(&self, swap: &mut Swap) -> error::Result<()> {
        let (Some(hash), Some(transfer_id)) =
            (swap.payment_hash.clone(), swap.spark_transfer_id.clone())
        else {
            error::bail!("direction B swap `{}` is missing its fields", swap.id());
        };
        let Some(preimage) = self.spark.revealed_preimage(&transfer_id).await? else {
            return Ok(());
        };
        log::info!(target: "swapd", "swap `{hash}` preimage revealed, settling the held payment");
        self.lampo.hold_claim(&preimage).await?;
        swap.transition(State::Done)?;
        self.store.persist(swap)?;
        Ok(())
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
                    let locked = claimable.iter().find(|htlc| htlc.payment_hash == hash);
                    // Their expiry is their choice. If it does not leave
                    // room to pay lightning and then claim, paying would
                    // mean paying the merchant and watching their lock
                    // refund out from under us.
                    if let Some(htlc) = locked {
                        if !expiry_leaves_room(htlc.expiry, CLAIM_MARGIN_SECS) {
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
                    if let Err(err) = self.settle_from_revealed_preimage(&mut swap).await {
                        log::error!(target: "swapd", "swap `{}`: {err}", swap.id());
                    } else if !swap.is_terminal()
                        && now() > swap.created_at + self.cfg.spark_htlc_expiry_secs
                    {
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

/// How much longer the held lightning payment must live than the spark
/// htlc it is paired with. We can only settle lightning *after* they
/// claim spark, so the lightning side has to be the last to expire.
const LN_HOLD_MARGIN_SECS: u64 = 3600;

/// How much of a spark htlc's remaining life we insist on before paying
/// the other leg, so learning the preimage still leaves time to claim.
const CLAIM_MARGIN_SECS: u64 = 600;

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
