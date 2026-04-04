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
use std::sync::Arc;
use std::time::Duration;

use lampo_common::bitcoin::hashes::sha256::Hash as Sha256;
use lampo_common::bitcoin::hashes::Hash;
use lampo_common::bitcoin::secp256k1::PublicKey as pubkey;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::keys::LampoKeysManager;
use lampo_common::ldk;
use lampo_common::ldk::ln::channelmanager::{
    Bolt11InvoiceParameters, PaymentId, RecipientOnionFields, Retry,
};
use lampo_common::ldk::offers::offer::Amount;
use lampo_common::ldk::offers::offer::Offer;
use lampo_common::ldk::routing::router::{PaymentParameters, RouteParameters, RouteParametersConfig};
use lampo_common::ldk::sign::EntropySource;
use lampo_common::ldk::types::payment::{PaymentHash, PaymentPreimage};

use super::LampoChannelManager;
use crate::chain::LampoChainManager;
use crate::utils::logger::LampoLogger;

pub struct OffchainManager {
    channel_manager: Arc<LampoChannelManager>,
    keys_manager: Arc<LampoKeysManager>,
    logger: Arc<LampoLogger>,
    lampo_conf: Arc<LampoConf>,
    chain_manager: Arc<LampoChainManager>,
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
        let desc = ldk::invoice::Description::new(description.to_string())
            .map_err(|err| error::anyhow!("{:?}", err))?;
        let params = Bolt11InvoiceParameters {
            amount_msats: amount_msat,
            description: ldk::invoice::Bolt11InvoiceDescription::Direct(desc),
            invoice_expiry_delta_secs: Some(expiring_in),
            ..Default::default()
        };
        let invoice = self.channel_manager.manager()
            .create_bolt11_invoice(params)
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
    ) -> error::Result<()> {
        // check if it is an invoice or an offer
        let offer_hash = Sha256::hash(offer_str.as_bytes());
        let payment_id = PaymentId(*offer_hash.as_ref());
        let offer = Offer::from_str(offer_str).map_err(|err| error::anyhow!("{:?}", err))?;

        let amount = match offer.amount() {
            Some(Amount::Bitcoin { amount_msats }) => {
                // Offer already specifies amount; pass None to let LDK use it,
                // but allow the caller to override with a different amount.
                amount_msat.or(Some(amount_msats.clone()))
            },
            Some(_) => error::bail!(
                "Cannot process non-Bitcoin-denominated offer value {:?}",
                offer.amount()
            ),
            None => Some(amount_msat.ok_or(error::anyhow!("An amount need to be specified"))?),
        };

        log::debug!(target: "lampo::offchain", "paying offer with amount `{:?}msat` & payer_note: `{}`", amount, payer_note.as_ref().unwrap_or(&"".to_string()));
        self.channel_manager
            .manager()
            .pay_for_offer(
                &offer,
                amount,
                payment_id,
                ldk::ln::channelmanager::OptionalOfferPaymentParams {
                    payer_note,
                    ..Default::default()
                },
            )
            .map_err(|err| error::anyhow!("{:?}", err))?;
        Ok(())
    }

    pub fn pay_invoice(&self, invoice_str: &str, amount_msat: Option<u64>) -> error::Result<()> {
        let invoice = self.decode_invoice(invoice_str)?;
        let payment_id = PaymentId((*invoice.payment_hash()).to_byte_array());
        self.channel_manager
            .manager()
            .pay_for_bolt11_invoice(
                &invoice,
                payment_id,
                amount_msat,
                RouteParametersConfig::default(),
                Retry::Attempts(10),
            )
            .map_err(|err| error::anyhow!("{:?}", err))?;
        Ok(())
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
                RecipientOnionFields::spontaneous_empty(),
                PaymentId(payment_hash.0),
                route_params,
                Retry::Timeout(Duration::from_secs(10)),
            )
            .map_err(|err| error::anyhow!("{:?}", err))?;
        log::info!("Keysend successfully done!");
        Ok(payment_result)
    }
}
