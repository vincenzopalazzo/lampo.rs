use std::sync::Arc;

use crate::chan;
use crate::error;
use crate::ldk;
use crate::event::Event;

pub trait Handler: Send + Sync {
    fn events(&self) -> chan::Receiver<Event>;
    fn emit(&self, event: Event);
}

pub trait EventForSpecificLDKVersion {
    fn handler(&self, handler: &dyn Handler, event: ldk::events::Event) -> error::Result<()>;
}

// This trait is helps to implement the underlying functions under `json_pay` for both
// the RGB and vanilla versions of `rust-lightning` as both of their implementation is different.
pub trait APIStrategy<T> {
    fn pay_invoice(
        &self,
        ctx: &T,
        invoice_str: &str,
        amount_msat: Option<u64>,
    ) -> error::Result<()>;

    fn pay_offer(&self, offer_str: &str, amount_msat: Option<u64>) -> error::Result<()>;

    fn generate_invoice(
        &self,
        amount_msat: Option<u64>,
        description: &str,
        expiring_in: u32,
    ) -> error::Result<ldk::invoice::Bolt11Invoice>;
}