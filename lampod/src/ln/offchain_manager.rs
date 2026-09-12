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
use lampo_common::currency::LampoCurrencyConversion;
use lampo_common::error;
use lampo_common::hex;
use lampo_common::ldk;
use lampo_common::ldk::blinded_path::message::BlindedMessagePath;
use lampo_common::ldk::ln::channelmanager::{
    Bolt11InvoiceParameters, OptionalBolt11PaymentParams, OptionalOfferPaymentParams, PaymentId,
};
use lampo_common::ldk::ln::outbound_payment::{RecipientOnionFields, Retry};
use lampo_common::ldk::offers::contacts::ContactSecrets;
use lampo_common::ldk::offers::nonce::Nonce;
use lampo_common::ldk::offers::offer::Amount;
use lampo_common::ldk::offers::offer::Offer;
use lampo_common::ldk::routing::router::{PaymentParameters, RouteParameters};
use lampo_common::ldk::sign::EntropySource;
use lampo_common::ldk::types::payment::{PaymentHash, PaymentPreimage};
use lampo_common::ldk::util::ser::Readable;
use lampo_common::signer::LampoSigner;

use super::LampoChannelManager;
use crate::utils::logger::LampoLogger;

pub struct OffchainManager {
    channel_manager: Arc<LampoChannelManager>,
    keys_manager: Arc<dyn LampoSigner>,
    logger: Arc<LampoLogger>,
    lampo_conf: Arc<LampoConf>,
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
        keys_manager: Arc<dyn LampoSigner>,
        channel_manager: Arc<LampoChannelManager>,
        logger: Arc<LampoLogger>,
        lampo_conf: Arc<LampoConf>,
    ) -> error::Result<Self> {
        let async_payments_enabled =
            Arc::new(AtomicBool::new(lampo_conf.async_payments_role.is_some()));
        let manager = Self {
            channel_manager,
            keys_manager,
            logger,
            lampo_conf,
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
        self.pay_offer_with_contact(offer_str, amount_msat, payer_note, max_fee_msat, None)
    }

    /// Pay a BOLT12 offer, optionally revealing BLIP-42 contact identity.
    pub fn pay_offer_with_contact(
        &self,
        offer_str: &str,
        amount_msat: Option<u64>,
        payer_note: Option<String>,
        max_fee_msat: Option<u64>,
        contact: Option<ContactPaymentParams>,
    ) -> error::Result<PaymentId> {
        // Same as ldk-node: a fresh random id per attempt. Hashing the offer
        // string collides on a second pay of the same offer, and a 1s retry
        // is too short for the static-invoice onion-message round trip
        // (ldk-node uses `Retry::Timeout(10s)`).
        let payment_id = PaymentId(self.keys_manager.get_secure_random_bytes());
        let offer = Offer::from_str(offer_str).map_err(|err| error::anyhow!("{:?}", err))?;

        let conversion = LampoCurrencyConversion::from_conf(&self.lampo_conf)?;
        // An explicit amount is checked against the offer before the invoice
        // request is sent. A currency offer with no amount lets the payee
        // price the invoice; the returned invoice is checked against the same
        // converter. Do not substitute a converted amount here: that would
        // hide the currency from the payee.
        let amount = match offer.amount() {
            Some(Amount::Bitcoin { .. }) | Some(Amount::Currency { .. }) => amount_msat,
            None => Some(amount_msat.ok_or(error::anyhow!("An amount need to be specified"))?),
        };

        let mut params = OptionalOfferPaymentParams {
            payer_note,
            // Compact-offer payback needs enough time for the invoice-request
            // onion round-trip plus route attempts on blinded payment paths.
            // Match the non-contact pay timeout (ldk-node uses 10s; we use 120s
            // so static-invoice round trips are not reported as failed early).
            retry_strategy: Retry::Timeout(Duration::from_secs(120)),
            route_params_config: ldk::routing::router::RouteParametersConfig {
                max_total_routing_fee_msat: max_fee_msat,
                ..Default::default()
            },
            ..Default::default()
        };
        if let Some(contact) = contact {
            let payer_offer_len = contact.payer_offer.as_ref().len();
            log::info!(
                target: "lampo::offchain",
                "BLIP-42 pay: payer_offer_tlv_len={payer_offer_len} (limit 300), has_secrets={}",
                contact.secrets.primary_secret().as_bytes().len() == 32
            );
            // Surface the size failure before LDK's opaque InvalidPayerOffer.
            if payer_offer_len > 300 {
                error::bail!(
                    "compact payer_offer is {payer_offer_len} bytes; BLIP-42 requires <= 300 (use BIP-353 return path)"
                );
            }
            params.contact_secrets = Some(contact.secrets);
            params.payer_offer = Some(contact.payer_offer);
        }

        log::info!(
            target: "lampo::offchain",
            "paying offer with amount `{:?}`, reveal_contact={}",
            amount,
            params.contact_secrets.is_some()
        );
        self.channel_manager
            .manager()
            .pay_for_offer_with_conversion(&offer, amount, payment_id, params, &conversion)
            .map_err(|err| error::anyhow!("{:?}", err))?;
        Ok(payment_id)
    }

    /// Build a compact payer offer + contact secrets for BLIP-42 reveal.
    pub fn build_contact_payment_params(
        &self,
        their_offer: &Offer,
        intro_node: pubkey,
        existing: Option<(Offer, Nonce, ContactSecrets)>,
    ) -> error::Result<ContactPaymentParams> {
        if let Some((payer_offer, nonce, secrets)) = existing {
            return Ok(ContactPaymentParams {
                secrets,
                payer_offer,
                nonce: Some(nonce),
            });
        }

        let manager = self.channel_manager.manager();
        // Compact payer offers must be <= 300 TLV bytes (BLIP-42). On signet the
        // explicit chain hash + blinded path can land just over the limit; retry a
        // few times in case path padding/nonce encoding varies, then fail clearly.
        let mut last_len = 0usize;
        for attempt in 0..8 {
            let (builder, nonce) = manager
                .create_compact_offer_builder(intro_node)
                .map_err(|err| error::anyhow!("create_compact_offer_builder: {:?}", err))?;
            let payer_offer = builder
                .build()
                .map_err(|err| error::anyhow!("build compact payer offer: {:?}", err))?;
            last_len = payer_offer.as_ref().len();
            log::info!(
                target: "lampo::offchain",
                "compact payer_offer attempt {attempt}: tlv_len={last_len}"
            );
            if last_len <= 300 {
                let secrets = manager
                    .compute_contact_secret(&payer_offer, nonce, their_offer)
                    .map_err(|err| error::anyhow!("compute_contact_secret: {:?}", err))?;
                return Ok(ContactPaymentParams {
                    secrets,
                    payer_offer,
                    nonce: Some(nonce),
                });
            }
        }
        error::bail!(
            "compact payer_offer stayed at {last_len} bytes after retries; BLIP-42 requires <= 300 (need BIP-353 or smaller blinded path / SCID intro)"
        );
    }

    /// Create a compact payer offer for BLIP-42 without deriving a new contact secret.
    /// Used when paying back a contact whose secret we already stored from an inbound payment.
    pub fn create_compact_payer_offer(&self, intro_node: pubkey) -> error::Result<(Offer, Nonce)> {
        let manager = self.channel_manager.manager();
        let (builder, nonce) = manager
            .create_compact_offer_builder(intro_node)
            .map_err(|err| error::anyhow!("create_compact_offer_builder: {:?}", err))?;
        let payer_offer = builder
            .build()
            .map_err(|err| error::anyhow!("build compact payer offer: {:?}", err))?;
        Ok((payer_offer, nonce))
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
        let payment_preimage = PaymentPreimage(self.keys_manager.get_secure_random_bytes());
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

/// Parameters required to reveal BLIP-42 contact identity on a BOLT12 pay.
pub struct ContactPaymentParams {
    pub secrets: ContactSecrets,
    pub payer_offer: Offer,
    pub nonce: Option<Nonce>,
}
