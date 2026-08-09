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
    pub async fn fetch_invoice(
        &self,
        offer: &str,
        amount_msat: Option<u64>,
    ) -> error::Result<response::FetchInvoiceResult> {
        self.handler
            .call(
                "fetchinvoice",
                request::FetchInvoice {
                    offer_str: offer.to_owned(),
                    amount_msat,
                    payer_note: None,
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

    /// Publish a fresh BOLT12 offer of ours (Direction B entry point).
    pub async fn create_offer(&self, amount_msat: Option<u64>) -> error::Result<response::Offer> {
        self.handler
            .call(
                "offer",
                request::GenerateOffer {
                    amount_msat,
                    description: Some("lampo-spark-swapd".to_owned()),
                },
            )
            .await
    }
}

/// The offer id LDK derives for an offer string: the merkle-root
/// derived `OfferId`, hex encoded. Used to correlate a
/// `PaymentClaimed` back to the Direction B swap that published the
/// offer.
pub fn offer_id(offer_str: &str) -> error::Result<String> {
    use std::str::FromStr;
    let offer = lampo_common::ldk::offers::offer::Offer::from_str(offer_str)
        .map_err(|err| error::anyhow!("invalid offer: {err:?}"))?;
    Ok(hex_encode(offer.id().0))
}

pub fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}
