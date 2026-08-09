//! Lampo Offchain manager.
//!
//! The offchain manager will manage all the necessary
//! information about the lightning network operation.
//!
//! Such as generate and invoice or pay an invoice.
//!
//! This module will also be able to interact with
//! other feature like onion message, and more general
//! with the network graph. But this is not so clear yet.
//!
//! Author: Vincenzo Palazzo <vincenzopalazzo@member.fsf.org>
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use lampo_common::bitcoin::hashes::sha256::Hash as Sha256;
use lampo_common::bitcoin::hashes::Hash;
use lampo_common::bitcoin::secp256k1::PublicKey as pubkey;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::keys::LampoKeysManager;
use lampo_common::ldk;
use lampo_common::ldk::blinded_path::message::OffersContext;
use lampo_common::ldk::ln::channelmanager::{
    Bolt11InvoiceParameters, OptionalBolt11PaymentParams, OptionalOfferPaymentParams, PaymentId,
};
use lampo_common::ldk::ln::outbound_payment::{RecipientOnionFields, Retry};
use lampo_common::ldk::offers::invoice::Bolt12Invoice;
use lampo_common::ldk::offers::offer::Amount;
use lampo_common::ldk::offers::offer::Offer;
use lampo_common::ldk::routing::router::{PaymentParameters, RouteParameters};
use lampo_common::ldk::sign::EntropySource;
use lampo_common::ldk::types::payment::{PaymentHash, PaymentPreimage};

use super::LampoChannelManager;
use crate::chain::LampoChainManager;
use crate::utils::logger::LampoLogger;

/// Why we asked the network for a BOLT12 invoice. Since lampo handles
/// BOLT12 invoices manually (see `default_ldk_conf`), every
/// `pay_for_offer` records its intent here and the `InvoiceReceived`
/// handler acts on it: `Pay` is settled right away, `Fetch` stores the
/// invoice for a later `payfetched`/`cancelfetched`, and an invoice
/// with no recorded intent (e.g. replayed after a restart) is
/// abandoned, never paid.
enum Bolt12Flow {
    Pay,
    Fetch {
        invoice: Option<(Bolt12Invoice, Option<OffersContext>)>,
    },
}

/// The outcome of an `InvoiceReceived` event, decided by
/// [`OffchainManager::on_invoice_received`].
pub struct InvoiceReceivedOutcome {
    /// Set when the invoice was stored by a `fetchinvoice` flow, so the
    /// handler can notify the waiting RPC.
    pub fetched: bool,
    pub payment_hash: PaymentHash,
    pub amount_msat: u64,
}

pub struct OffchainManager {
    channel_manager: Arc<LampoChannelManager>,
    keys_manager: Arc<LampoKeysManager>,
    logger: Arc<LampoLogger>,
    lampo_conf: Arc<LampoConf>,
    chain_manager: Arc<LampoChainManager>,
    bolt12_flows: Mutex<HashMap<PaymentId, Bolt12Flow>>,
}

impl OffchainManager {
    // FIXME: use the build pattern here
    pub fn new(
        keys_manager: Arc<LampoKeysManager>,
        channel_manager: Arc<LampoChannelManager>,
        logger: Arc<LampoLogger>,
        lampo_conf: Arc<LampoConf>,
        chain_manager: Arc<LampoChainManager>,
    ) -> error::Result<Self> {
        Ok(Self {
            channel_manager,
            keys_manager,
            logger,
            lampo_conf,
            chain_manager,
            bolt12_flows: Mutex::new(HashMap::new()),
        })
    }

    fn entropy(&self) -> [u8; 32] {
        self.chain_manager
            .wallet_manager
            .ldk_keys()
            .keys_manager
            .clone()
            .get_secure_random_bytes()
    }

    /// Resolve the amount to pay for an offer, either from the offer
    /// itself or from the caller.
    fn offer_amount(offer: &Offer, amount_msat: Option<u64>) -> error::Result<u64> {
        match offer.amount() {
            Some(Amount::Bitcoin { amount_msats }) => Ok(amount_msats),
            Some(_) => error::bail!(
                "Cannot process non-Bitcoin-denominated offer value {:?}",
                offer.amount()
            ),
            None => amount_msat.ok_or(error::anyhow!("An amount need to be specified")),
        }
    }

    /// Generate an invoice with a specific amount and a specific
    /// description.
    pub fn generate_invoice(
        &self,
        amount_msat: Option<u64>,
        description: &str,
        expiring_in: u32,
    ) -> error::Result<ldk::invoice::Bolt11Invoice> {
        let description = ldk::invoice::Bolt11InvoiceDescription::Direct(
            ldk::invoice::Description::new(description.to_string())
                .map_err(|err| error::anyhow!("{:?}", err))?,
        );
        let invoice = self
            .channel_manager
            .manager()
            .create_bolt11_invoice(Bolt11InvoiceParameters {
                amount_msats: amount_msat,
                description,
                invoice_expiry_delta_secs: Some(expiring_in),
                ..Default::default()
            })
            .map_err(|err| error::anyhow!("{:?}", err))?;
        Ok(invoice)
    }

    /// Generate an invoice for an external payment hash. The node does
    /// not know the preimage, so the payment will be held when it
    /// arrives (see `HoldManager`).
    ///
    /// The `min_final_cltv_expiry_delta` bounds how long (in blocks)
    /// the payment can be held: LDK fails the pending HTLCs back
    /// roughly 39 blocks before their expiry, so with the default
    /// delta of 42 blocks the hold window is only ~3 blocks.
    pub fn generate_invoice_for_hash(
        &self,
        payment_hash: PaymentHash,
        amount_msat: Option<u64>,
        description: &str,
        expiring_in: u32,
        min_final_cltv_expiry_delta: Option<u16>,
    ) -> error::Result<ldk::invoice::Bolt11Invoice> {
        let description = ldk::invoice::Bolt11InvoiceDescription::Direct(
            ldk::invoice::Description::new(description.to_string())
                .map_err(|err| error::anyhow!("{:?}", err))?,
        );
        let invoice = self
            .channel_manager
            .manager()
            .create_bolt11_invoice(Bolt11InvoiceParameters {
                amount_msats: amount_msat,
                description,
                invoice_expiry_delta_secs: Some(expiring_in),
                min_final_cltv_expiry_delta,
                payment_hash: Some(payment_hash),
                ..Default::default()
            })
            .map_err(|err| error::anyhow!("{:?}", err))?;
        Ok(invoice)
    }

    pub fn decode_invoice(&self, invoice_str: &str) -> error::Result<ldk::invoice::Bolt11Invoice> {
        // FIXME: we should be able to `?` on the error right?
        let invoice = invoice_str
            .parse::<ldk::invoice::Bolt11Invoice>()
            .map_err(|er| error::anyhow!("{:?}", er))?;
        Ok(invoice)
    }

    pub fn decode<T: FromStr>(&self, invoice_str: &str) -> error::Result<T> {
        let invoice = invoice_str
            .parse::<T>()
            .map_err(|_| error::anyhow!("Impossible decode the invoice `{invoice_str}`"))?;
        Ok(invoice)
    }

    pub fn pay_offer(
        &self,
        offer_str: &str,
        amount_msat: Option<u64>,
        payer_note: Option<String>,
    ) -> error::Result<PaymentId> {
        // check if it is an invoice or an offer
        let offer_hash = Sha256::hash(offer_str.as_bytes());
        let payment_id = PaymentId(*offer_hash.as_ref());
        let offer = Offer::from_str(offer_str).map_err(|err| error::anyhow!("{:?}", err))?;
        let amount = Self::offer_amount(&offer, amount_msat)?;

        log::debug!(target: "lampo::offchain", "paying offer with amount `{}msat` & payer_note: `{}`", amount, payer_note.as_ref().unwrap_or(&"".to_string()));
        // record the intent first: the `InvoiceReceived` handler pays
        // only invoices it can attribute to a `pay` call.
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        self.bolt12_flows
            .lock()
            .unwrap()
            .insert(payment_id, Bolt12Flow::Pay);
        let result = self.channel_manager.manager().pay_for_offer(
            &offer,
            Some(amount),
            payment_id,
            OptionalOfferPaymentParams {
                payer_note,
                retry_strategy: Retry::Timeout(std::time::Duration::from_secs(1)),
                ..Default::default()
            },
        );
        if let Err(err) = result {
            self.bolt12_flows.lock().unwrap().remove(&payment_id);
            error::bail!("{:?}", err);
        }
        Ok(payment_id)
    }

    /// Ask the offer issuer for a BOLT12 invoice *without* paying it.
    /// The invoice arrives asynchronously via `InvoiceReceived` and is
    /// stored until `pay_fetched_invoice` or `cancel_fetched_invoice`.
    ///
    /// N.B: LDK garbage collects the pending payment roughly one
    /// minute after the invoice request goes out, so the fetched
    /// invoice must be paid (or it expires) within that window.
    pub fn fetch_invoice_from_offer(
        &self,
        offer_str: &str,
        amount_msat: Option<u64>,
        payer_note: Option<String>,
    ) -> error::Result<PaymentId> {
        let payment_id = PaymentId(self.entropy());
        let offer = Offer::from_str(offer_str).map_err(|err| error::anyhow!("{:?}", err))?;
        let amount = Self::offer_amount(&offer, amount_msat)?;

        log::debug!(target: "lampo::offchain", "fetching invoice for offer with amount `{amount}msat`");
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        self.bolt12_flows
            .lock()
            .unwrap()
            .insert(payment_id, Bolt12Flow::Fetch { invoice: None });
        let result = self.channel_manager.manager().pay_for_offer(
            &offer,
            Some(amount),
            payment_id,
            OptionalOfferPaymentParams {
                payer_note,
                retry_strategy: Retry::Timeout(std::time::Duration::from_secs(1)),
                ..Default::default()
            },
        );
        if let Err(err) = result {
            self.bolt12_flows.lock().unwrap().remove(&payment_id);
            error::bail!("{:?}", err);
        }
        Ok(payment_id)
    }

    /// Pay an invoice previously stored by `fetch_invoice_from_offer`,
    /// returning the payment hash the caller should wait on.
    pub fn pay_fetched_invoice(&self, payment_id: PaymentId) -> error::Result<PaymentHash> {
        // Take the invoice only once we know there is one: removing the
        // entry for a fetch still in flight would drop the pending
        // request and make its invoice arrive unattributed.
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        let entry = {
            let mut flows = self.bolt12_flows.lock().unwrap();
            match flows.get(&payment_id) {
                Some(Bolt12Flow::Fetch {
                    invoice: Some(_), ..
                }) => flows.remove(&payment_id),
                Some(Bolt12Flow::Fetch { invoice: None }) => {
                    error::bail!("the invoice for payment id `{payment_id}` has not arrived yet")
                }
                _ => None,
            }
        };
        let Some(Bolt12Flow::Fetch {
            invoice: Some((invoice, context)),
        }) = entry
        else {
            error::bail!("no fetched invoice found for payment id `{payment_id}`");
        };
        let payment_hash = invoice.payment_hash();
        if let Err(err) = self
            .channel_manager
            .manager()
            .send_payment_for_bolt12_invoice(&invoice, context.as_ref())
        {
            // Drop the pending payment as well, the caller has no
            // handle to retry it with.
            self.channel_manager.manager().abandon_payment(payment_id);
            error::bail!(
                "{:?} (the invoice request may have expired, fetch the invoice again)",
                err
            );
        }
        Ok(payment_hash)
    }

    /// Abandon an invoice previously fetched with `fetch_invoice_from_offer`.
    ///
    /// Only fetch flows are cancellable: the payment id of an in-flight
    /// `pay` must not be abandoned through this path.
    pub fn cancel_fetched_invoice(&self, payment_id: PaymentId) -> error::Result<()> {
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        let mut flows = self.bolt12_flows.lock().unwrap();
        match flows.get(&payment_id) {
            Some(Bolt12Flow::Fetch { .. }) => {
                flows.remove(&payment_id);
            }
            Some(Bolt12Flow::Pay) => {
                error::bail!("payment id `{payment_id}` belongs to a pay call, not a fetch")
            }
            None => error::bail!("no fetched invoice found for payment id `{payment_id}`"),
        }
        drop(flows);
        self.channel_manager.manager().abandon_payment(payment_id);
        Ok(())
    }

    /// React to a manually handled BOLT12 invoice. Called from the LDK
    /// event handler, it must never block. Invoices that cannot be
    /// attributed to a `pay` or `fetchinvoice` call (e.g. replayed
    /// after a restart) are abandoned: paying them without an intent
    /// record would move funds nobody asked to move.
    pub fn on_invoice_received(
        &self,
        payment_id: PaymentId,
        invoice: Bolt12Invoice,
        context: Option<OffersContext>,
    ) -> error::Result<Option<InvoiceReceivedOutcome>> {
        let payment_hash = invoice.payment_hash();
        let amount_msat = invoice.amount_msats();
        // SAFETY: the mutex is never poisoned, we do not panic while holding it.
        let mut flows = self.bolt12_flows.lock().unwrap();
        match flows.get_mut(&payment_id) {
            Some(Bolt12Flow::Pay) => {
                flows.remove(&payment_id);
                drop(flows);
                if let Err(err) = self
                    .channel_manager
                    .manager()
                    .send_payment_for_bolt12_invoice(&invoice, context.as_ref())
                {
                    // Abandon the payment so LDK emits `PaymentFailed`:
                    // without it the `pay` caller waits for a payment
                    // event that would never come.
                    self.channel_manager.manager().abandon_payment(payment_id);
                    error::bail!("{:?}", err);
                }
                Ok(Some(InvoiceReceivedOutcome {
                    fetched: false,
                    payment_hash,
                    amount_msat,
                }))
            }
            Some(Bolt12Flow::Fetch {
                invoice: stored @ None,
            }) => {
                *stored = Some((invoice, context));
                Ok(Some(InvoiceReceivedOutcome {
                    fetched: true,
                    payment_hash,
                    amount_msat,
                }))
            }
            Some(Bolt12Flow::Fetch { invoice: Some(_) }) => {
                // Keep the first invoice. The fetch already reported its
                // payment hash to the caller, and paying a later one
                // would move funds under a different hash.
                log::warn!(
                    target: "lampo::offchain",
                    "ignoring a second bolt12 invoice for payment id `{payment_id}`"
                );
                Ok(None)
            }
            None => {
                log::warn!(
                    target: "lampo::offchain",
                    "abandoning bolt12 invoice for unknown payment id `{payment_id}`"
                );
                drop(flows);
                self.channel_manager.manager().abandon_payment(payment_id);
                Ok(None)
            }
        }
    }

    pub fn pay_invoice(
        &self,
        invoice_str: &str,
        amount_msat: Option<u64>,
    ) -> error::Result<PaymentId> {
        // check if it is an invoice or an offer
        let invoice = self.decode_invoice(invoice_str)?;
        let payment_id = PaymentId(invoice.payment_hash().0);
        // Only forward a caller-supplied amount for zero-amount invoices. For a
        // fixed-amount invoice LDK treats `amount_msat` as an overpayment, so
        // drop it (matching the pre-0.3 `payment_parameters_from_invoice`).
        let amount_msat = if invoice.amount_milli_satoshis().is_some() {
            None
        } else {
            amount_msat
        };
        self.channel_manager
            .manager()
            .pay_for_bolt11_invoice(
                &invoice,
                payment_id,
                amount_msat,
                OptionalBolt11PaymentParams {
                    retry_strategy: Retry::Attempts(10),
                    ..Default::default()
                },
            )
            .map_err(|err| error::anyhow!("{:?}", err))?;
        Ok(payment_id)
    }

    pub fn keysend(&self, destination: pubkey, amount_msat: u64) -> error::Result<PaymentHash> {
        let payment_preimage = PaymentPreimage(
            self.chain_manager
                .wallet_manager
                .ldk_keys()
                .keys_manager
                .clone()
                .get_secure_random_bytes(),
        );
        let PaymentPreimage(bytes) = payment_preimage;
        let payment_hash = PaymentHash(Sha256::hash(&bytes).to_byte_array());
        // The 40 here is the max CheckLockTimeVerify which locks the output of the transaction for a certain
        // period of time.The false here stands for the allow_mpp, which is to allow the multi part route payments.
        let route_params = RouteParameters {
            payment_params: PaymentParameters::for_keysend(destination, 40, false),
            final_value_msat: amount_msat,
            max_total_routing_fee_msat: None,
        };
        log::info!("Initialised Keysend");
        let payment_result = self
            .channel_manager
            .manager()
            .send_spontaneous_payment(
                Some(payment_preimage),
                RecipientOnionFields::spontaneous_empty(amount_msat),
                PaymentId(payment_hash.0),
                route_params,
                Retry::Timeout(Duration::from_secs(10)),
            )
            .map_err(|err| error::anyhow!("{:?}", err))?;
        log::info!("Keysend successfully done!");
        Ok(payment_result)
    }
}
