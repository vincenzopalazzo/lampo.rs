//! The lightning leg: a thin, typed wrapper over the in-process
//! `LampoHandler`. Everything here is the public model of the node's
//! own JSON-RPC surface, called directly — no HTTP hop.
use std::sync::Arc;

use lampo_common::chan::UnboundedReceiver;
use lampo_common::error;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::model::{request, response};
use lampod::actions::handler::LampoHandler;

pub struct LampoLeg {
    handler: Arc<LampoHandler>,
}

impl LampoLeg {
    pub fn new(handler: Arc<LampoHandler>) -> Self {
        Self { handler }
    }

    /// Live event stream of the node: `PaymentHeld`,
    /// `Bolt12InvoiceReceived`, payment events and the raw LDK feed.
    pub fn events(&self) -> UnboundedReceiver<Event> {
        self.handler.events()
    }

    /// Fetch a BOLT12 invoice from an offer without paying it. The
    /// returned payment hash is what the Spark leg locks on.
    ///
    /// `max_cltv_expiry_delta` caps how long the eventual payment can
    /// stay in flight, in blocks. Without it a stuck payment can outlive
    /// the counterparty's Spark HTLC: it refunds to them, then our
    /// payment settles, and we have paid with nothing left to claim.
    pub async fn fetch_invoice(
        &self,
        offer: &str,
        amount_msat: Option<u64>,
        max_cltv_expiry_delta: u32,
    ) -> error::Result<response::FetchInvoiceResult> {
        self.handler
            .call(
                "fetchinvoice",
                request::FetchInvoice {
                    offer_str: offer.to_owned(),
                    amount_msat,
                    payer_note: None,
                    max_cltv_expiry_delta: Some(max_cltv_expiry_delta),
                },
            )
            .await
    }

    /// Pay a previously fetched invoice; the result carries the
    /// preimage on success.
    pub async fn pay_fetched(&self, payment_id: &str) -> error::Result<response::PayResult> {
        self.handler
            .call(
                "payfetched",
                request::PayFetched {
                    payment_id: payment_id.to_owned(),
                },
            )
            .await
    }

    pub async fn cancel_fetched(&self, payment_id: &str) -> error::Result<()> {
        let _: response::CancelFetchedResult = self
            .handler
            .call(
                "cancelfetched",
                request::CancelFetched {
                    payment_id: payment_id.to_owned(),
                },
            )
            .await?;
        Ok(())
    }

    /// The preimage of an outbound payment, if the node settled it and
    /// still holds the receipt. `None` means unknown, pending or failed:
    /// the caller cannot distinguish those, only that no preimage is
    /// available. Used to recover a Direction A swap that crashed
    /// mid-payment, where the node kept the preimage swapd lost.
    pub async fn payment_preimage(&self, payment_hash: &str) -> error::Result<Option<String>> {
        let result: response::PaymentPreimageResult = self
            .handler
            .call(
                "paymentpreimage",
                request::PaymentPreimage {
                    payment_hash: payment_hash.to_owned(),
                },
            )
            .await?;
        Ok(result.payment_preimage)
    }

    /// Issue a hold invoice on a payment hash *the counterparty chose*.
    /// We cannot settle it: only they know the preimage. That is the
    /// whole point, it is what makes the receive direction atomic.
    ///
    /// `min_final_cltv_expiry_delta` bounds how long the payment can be
    /// held, and must leave room for the spark leg to expire first.
    pub async fn hold_invoice(
        &self,
        payment_hash: &str,
        amount_msat: u64,
        min_final_cltv_expiry_delta: u16,
        expiring_in: u32,
    ) -> error::Result<response::HoldInvoiceResult> {
        self.handler
            .call(
                "holdinvoice",
                request::HoldInvoice {
                    payment_hash: payment_hash.to_owned(),
                    amount_msat: Some(amount_msat),
                    description: "lampo-spark-swapd".to_owned(),
                    expiring_in: Some(expiring_in),
                    min_final_cltv_expiry_delta: Some(min_final_cltv_expiry_delta),
                },
            )
            .await
    }

    /// Settle a held payment with the preimage the counterparty
    /// revealed by claiming their spark htlc.
    pub async fn hold_claim(&self, preimage: &str) -> error::Result<()> {
        let _: response::HoldClaimResult = self
            .handler
            .call(
                "holdclaim",
                request::HoldClaim {
                    payment_preimage: preimage.to_owned(),
                },
            )
            .await?;
        Ok(())
    }

    /// Give a held payment back to the payer.
    pub async fn hold_fail(&self, payment_hash: &str) -> error::Result<()> {
        let _: response::HoldFailResult = self
            .handler
            .call(
                "holdfail",
                request::HoldFail {
                    payment_hash: payment_hash.to_owned(),
                },
            )
            .await?;
        Ok(())
    }

    /// The holds the node knows about, so the engine can reconcile
    /// against the node rather than trusting its own record alone.
    pub async fn list_holds(&self) -> error::Result<Vec<response::Hold>> {
        let holds: response::ListHoldsResult = self
            .handler
            .call("listholds", request::ListHolds {})
            .await?;
        Ok(holds.holds)
    }
}
