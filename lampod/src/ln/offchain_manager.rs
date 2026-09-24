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
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lampo_common::bitcoin::hashes::sha256::Hash as Sha256;
use lampo_common::bitcoin::hashes::Hash;
use lampo_common::bitcoin::secp256k1::PublicKey as pubkey;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::hex;
use lampo_common::keys::LampoKeysManager;
use lampo_common::ldk;
use lampo_common::ldk::blinded_path::message::BlindedMessagePath;
use lampo_common::ldk::ln::channelmanager::{
    Bolt11InvoiceParameters, OptionalBolt11PaymentParams, OptionalOfferPaymentParams, PaymentId,
    RecurrenceCancellationParams, RecurrencePaymentParams,
};
use lampo_common::ldk::ln::outbound_payment::{RecipientOnionFields, Retry};
use lampo_common::ldk::offers::invoice_request::RecurrenceId;
use lampo_common::ldk::offers::offer::Amount;
use lampo_common::ldk::offers::offer::Offer;
use lampo_common::ldk::offers::offer::{Recurrence, RecurrenceType};
use lampo_common::ldk::routing::router::{PaymentParameters, RouteParameters};
use lampo_common::ldk::sign::EntropySource;
use lampo_common::ldk::types::payment::{PaymentHash, PaymentPreimage};
use lampo_common::ldk::util::ser::Readable;

use super::recurrence::{series_key, RecurrenceCadence, RecurrenceSeries, RecurrenceStore};
use super::LampoChannelManager;
use crate::chain::LampoChainManager;
use crate::persistence::LampoPersistence;
use crate::utils::logger::LampoLogger;

pub struct OffchainManager {
    channel_manager: Arc<LampoChannelManager>,
    keys_manager: Arc<LampoKeysManager>,
    logger: Arc<LampoLogger>,
    lampo_conf: Arc<LampoConf>,
    chain_manager: Arc<LampoChainManager>,
    recurrence: RecurrenceStore,
    /// Set once this node is configured as an often-offline async recipient,
    /// either from `async-invoice-server-paths` in the config or from a
    /// runtime [`Self::set_async_receive_paths`] call.
    async_receive_enabled: AtomicBool,
    /// Shared with the onion messenger: false until the operator opts in
    /// via `async-payments-role`, config paths, or `setasyncinvoicepaths`.
    async_payments_enabled: Arc<AtomicBool>,
}

impl OffchainManager {
    // FIXME: use the build pattern here
    pub fn new(
        keys_manager: Arc<LampoKeysManager>,
        channel_manager: Arc<LampoChannelManager>,
        logger: Arc<LampoLogger>,
        lampo_conf: Arc<LampoConf>,
        chain_manager: Arc<LampoChainManager>,
        persister: Arc<LampoPersistence>,
    ) -> error::Result<Self> {
        let async_payments_enabled =
            Arc::new(AtomicBool::new(lampo_conf.async_payments_role.is_some()));
        let manager = Self {
            channel_manager,
            keys_manager,
            logger,
            lampo_conf,
            chain_manager,
            recurrence: RecurrenceStore::new(persister),
            async_receive_enabled: AtomicBool::new(false),
            async_payments_enabled,
        };
        if let Some(paths_hex) = &manager.lampo_conf.async_invoice_server_paths {
            manager.set_async_receive_paths_hex(paths_hex)?;
        }
        Ok(manager)
    }

    /// Whether this node receives async payments as an often-offline recipient.
    pub fn async_receive_enabled(&self) -> bool {
        self.async_receive_enabled.load(Ordering::Acquire)
    }

    /// Shared with the onion messenger so a later opt-in flips the same flag.
    pub(crate) fn async_payments_gate(&self) -> Arc<AtomicBool> {
        self.async_payments_enabled.clone()
    }

    /// Configure this node as an often-offline async recipient with blinded
    /// paths to its static invoice server, obtained out-of-band from the
    /// server operator.
    pub fn set_async_receive_paths(&self, paths: Vec<BlindedMessagePath>) -> error::Result<()> {
        self.channel_manager
            .manager()
            .set_paths_to_static_invoice_server(paths)
            .map_err(|_| error::anyhow!("invalid async invoice server paths"))?;
        self.async_receive_enabled.store(true, Ordering::Release);
        self.async_payments_enabled.store(true, Ordering::Release);
        Ok(())
    }

    /// Same as [`Self::set_async_receive_paths`], taking the hex encoding
    /// produced by the `asyncinvoicepaths` RPC (or written in
    /// `async-invoice-server-paths`).
    pub fn set_async_receive_paths_hex(&self, paths_hex: &str) -> error::Result<()> {
        // A few blinded paths encode to well under this; reject oversized
        // hex before allocating the decoded buffer.
        const MAX_PATHS_HEX_LEN: usize = 16 * 1024;
        if paths_hex.len() > MAX_PATHS_HEX_LEN {
            error::bail!("async-invoice-server-paths hex is too long");
        }
        let bytes = hex::decode(paths_hex)
            .map_err(|err| error::anyhow!("async-invoice-server-paths is not hex: {err}"))?;
        let paths = <Vec<BlindedMessagePath>>::read(&mut &bytes[..]).map_err(|err| {
            error::anyhow!("async-invoice-server-paths is not a valid path list: {err:?}")
        })?;
        if paths.is_empty() {
            error::bail!("async-invoice-server-paths must not be empty");
        }
        self.set_async_receive_paths(paths)
    }

    /// The async receive offer, ready once the interactive static-invoice
    /// flow with the server has completed.
    pub fn async_offer(&self) -> error::Result<Offer> {
        self.channel_manager
            .manager()
            .get_async_receive_offer()
            .map_err(|_| {
                error::anyhow!(
                    "async receive offer not ready yet; the static invoice server handshake is still in flight"
                )
            })
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
        max_fee_msat: Option<u64>,
    ) -> error::Result<PaymentId> {
        // Same as ldk-node: a fresh random id per attempt. Hashing the offer
        // string collides on a second pay of the same offer, and a 1s retry
        // is too short for the static-invoice onion-message round trip
        // (ldk-node uses `Retry::Timeout(10s)`).
        let payment_id = PaymentId(self.keys_manager.get_secure_random_bytes());
        let offer = Offer::from_str(offer_str).map_err(|err| error::anyhow!("{:?}", err))?;

        let amount = match offer.amount() {
            Some(Amount::Bitcoin { amount_msats }) => amount_msats.clone(),
            Some(_) => error::bail!(
                "Cannot process non-Bitcoin-denominated offer value {:?}",
                offer.amount()
            ),
            None => amount_msat.ok_or(error::anyhow!("An amount need to be specified"))?,
        };

        log::debug!(target: "lampo::offchain", "paying offer with amount `{}msat` & payer_note: `{}`", amount, payer_note.as_ref().unwrap_or(&"".to_string()));
        self.channel_manager
            .manager()
            .pay_for_offer(
                &offer,
                Some(amount),
                payment_id,
                OptionalOfferPaymentParams {
                    payer_note,
                    // json_pay waits up to the RPC timeout for a terminal
                    // event. Retry must cover that window: a 1s/10s timeout
                    // (ldk-node can use 10s because it returns PaymentId
                    // immediately) expires the HTLC while we are still
                    // waiting, so the waiter never sees Success.
                    retry_strategy: Retry::Timeout(Duration::from_secs(120)),
                    route_params_config: ldk::routing::router::RouteParametersConfig {
                        max_total_routing_fee_msat: max_fee_msat,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .map_err(|err| error::anyhow!("{:?}", err))?;
        Ok(payment_id)
    }

    /// Pay one period of a recurring offer, starting or continuing the series
    /// tracked for this offer. Returns the payment id and the hex recurrence
    /// id (stable across all periods of the series).
    ///
    /// The series is persisted before calling LDK, so a crash between the
    /// two still resumes with the same recurrence id.
    pub fn pay_recurring_offer(
        &self,
        offer_str: &str,
        amount_msat: Option<u64>,
        payer_note: Option<String>,
        max_fee_msat: Option<u64>,
        cadence: RecurrenceCadence,
    ) -> error::Result<(PaymentId, String)> {
        let offer = Offer::from_str(offer_str).map_err(|err| error::anyhow!("{:?}", err))?;
        let offer_recurrence = offer.offer_recurrence().ok_or(error::anyhow!(
            "offer is not a recurring offer; drop the recurrence flag for a one-shot payment"
        ))?;
        if offer_recurrence.recurrence_period != cadence.to_ldk_period() {
            error::bail!(
                "offer recurs {:?} but pay asked for `{}`",
                offer_recurrence.recurrence_period,
                cadence.as_str()
            );
        }

        let amount = match offer.amount() {
            Some(Amount::Bitcoin { amount_msats }) => amount_msats.clone(),
            Some(_) => error::bail!(
                "Cannot process non-Bitcoin-denominated offer value {:?}",
                offer.amount()
            ),
            None => amount_msat.ok_or(error::anyhow!("An amount need to be specified"))?,
        };

        let key = series_key(&offer);
        let mut series = match self.recurrence.load(&key)? {
            Some(series) => series,
            None => {
                let fresh = RecurrenceSeries::new(self.keys_manager.get_secure_random_bytes());
                self.recurrence.save(&key, &fresh)?;
                fresh
            }
        };
        if series.cancelled {
            error::bail!("recurrence series for this offer was cancelled");
        }
        if series.pending_payment.is_some() {
            error::bail!("a recurring payment for this offer is already in flight");
        }

        let recurrence_id = RecurrenceId(series.recurrence_id);
        let payment_id = PaymentId(self.keys_manager.get_secure_random_bytes());
        log::debug!(target: "lampo::offchain", "paying recurring offer period `{}` with amount `{}`msat", series.next_counter, amount);
        self.channel_manager
            .manager()
            .pay_for_recurrence(
                &offer,
                Some(amount),
                payment_id,
                recurrence_id,
                RecurrencePaymentParams {
                    counter: series.next_counter,
                    start: series.start,
                    prev_state: series.prev_state.clone(),
                    quantity: None,
                    expected_invoice_recurrence_basetime: series.expected_basetime,
                },
                OptionalOfferPaymentParams {
                    payer_note,
                    retry_strategy: Retry::Timeout(Duration::from_secs(120)),
                    route_params_config: ldk::routing::router::RouteParametersConfig {
                        max_total_routing_fee_msat: max_fee_msat,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .map_err(|err| error::anyhow!("{:?}", err))?;

        series.pending_payment = Some(payment_id.0);
        self.recurrence.save(&key, &series)?;
        Ok((payment_id, hex::encode(series.recurrence_id)))
    }

    /// Cancel the tracked recurrence series for this offer. The counter must
    /// be nonzero, so a series that never paid has nothing to cancel.
    /// Returns the hex recurrence id.
    pub fn cancel_recurring_offer(&self, offer_str: &str) -> error::Result<String> {
        let offer = Offer::from_str(offer_str).map_err(|err| error::anyhow!("{:?}", err))?;
        let key = series_key(&offer);
        let mut series = self.recurrence.load(&key)?.ok_or(error::anyhow!(
            "no recurrence series tracked for this offer"
        ))?;
        if series.cancelled {
            error::bail!("recurrence series for this offer is already cancelled");
        }
        if series.pending_payment.is_some() {
            error::bail!(
                "a recurring payment for this offer is in flight; wait for it to settle first"
            );
        }
        if series.next_counter == 0 {
            error::bail!("series never paid; nothing to cancel");
        }

        self.channel_manager
            .manager()
            .cancel_recurrence(
                &offer,
                RecurrenceId(series.recurrence_id),
                RecurrenceCancellationParams {
                    counter: series.next_counter,
                    start: series.start,
                    prev_state: series.prev_state.clone(),
                },
            )
            .map_err(|err| error::anyhow!("{:?}", err))?;

        series.cancelled = true;
        self.recurrence.save(&key, &series)?;
        Ok(hex::encode(series.recurrence_id))
    }

    /// Build an offer-level recurrence descriptor for the `offer` RPC flag.
    /// v1 only advertises optional recurrence: one-shot payers keep working.
    pub fn ldk_recurrence(cadence: RecurrenceCadence) -> Recurrence {
        Recurrence {
            recurrence_type: RecurrenceType::Optional,
            recurrence_period: cadence.to_ldk_period(),
            recurrence_paywindow: None,
            recurrence_limit: None,
        }
    }

    pub fn pay_invoice(
        &self,
        invoice_str: &str,
        amount_msat: Option<u64>,
        max_fee_msat: Option<u64>,
        retry_timeout_secs: Option<u64>,
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
                    retry_strategy: retry_timeout_secs
                        .map(|seconds| Retry::Timeout(Duration::from_secs(seconds)))
                        .unwrap_or(Retry::Attempts(10)),
                    route_params_config: ldk::routing::router::RouteParametersConfig {
                        max_total_routing_fee_msat: max_fee_msat,
                        ..Default::default()
                    },
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
