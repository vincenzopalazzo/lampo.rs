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
use lampo_common::ldk::ln::channelmanager::Retry;
use lampo_common::ldk::ln::channelmanager::{PaymentId, RecipientOnionFields};
use lampo_common::ldk::ln::{PaymentHash, PaymentPreimage};
use lampo_common::ldk::offers::offer::Amount;
use lampo_common::ldk::offers::offer::Offer;
use lampo_common::ldk::routing::router::{PaymentParameters, RouteParameters};
use lampo_common::ldk::sign::EntropySource;

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
        let currency = ldk::invoice::Currency::try_from(self.lampo_conf.network)?;
        let invoice = ldk::ln::invoice_utils::create_invoice_from_channelmanager(
            &self.channel_manager.manager(),
            self.keys_manager.clone(),
            self.logger.clone(),
            currency,
            amount_msat,
            description.to_string(),
            expiring_in,
            None,
        )
        .map_err(|err| error::anyhow!(err))?;
        Ok(invoice)
    }

    pub fn decode_invoice(&self, invoice_str: &str) -> error::Result<ldk::invoice::Bolt11Invoice> {
        let invoice = invoice_str
            .parse::<ldk::invoice::Bolt11Invoice>()
            .map_err(|err| error::anyhow!("Error occured while decoding invoice {err}"))?;
        Ok(invoice)
    }

    pub fn decode<T: FromStr>(&self, invoice_str: &str) -> error::Result<T> {
        let invoice = invoice_str
            .parse::<T>()
            .map_err(|_| error::anyhow!("Impossible decode the invoice `{invoice_str}`"))?;
        Ok(invoice)
    }

    pub fn pay_offer(&self, offer_str: &str, amount_msat: Option<u64>) -> error::Result<()> {
        // check if it is an invoice or an offer
        let offer_hash = Sha256::hash(offer_str.as_bytes());
        let payment_id = PaymentId(*offer_hash.as_ref());
        let offer = Offer::from_str(offer_str).map_err(|err| error::anyhow!("{:?}", err))?;

        let amount = match offer.amount() {
            Some(Amount::Bitcoin { amount_msats }) => amount_msats.clone(),
            Some(_) => error::bail!(
                "Cannot process non-Bitcoin-denominated offer value {:?}",
                offer.amount()
            ),
            None => amount_msat.ok_or(error::anyhow!("An amount need to be specified"))?,
        };

        self.channel_manager
            .manager()
            .pay_for_offer(
                &offer,
                None,
                Some(amount),
                None,
                payment_id,
                Retry::Attempts(10),
                None,
            )
            .map_err(|err| error::anyhow!("{:?}", err))?;
        Ok(())
    }

    pub fn pay_invoice(&self, invoice_str: &str, amount_msat: Option<u64>) -> error::Result<()> {
        // check if it is an invoice or an offer
        let invoice = self.decode_invoice(invoice_str)?;
        let payment_id = PaymentId((*invoice.payment_hash()).to_byte_array());
        let (payment_hash, onion, route) = if invoice.amount_milli_satoshis().is_none() {
            ldk::ln::bolt11_payment::payment_parameters_from_zero_amount_invoice(
                &invoice,
                amount_msat.ok_or(error::anyhow!(
                    "invoice with no amount, and amount must be specified"
                ))?,
            )
            .map_err(|err| error::anyhow!("{:?}", err))?
        } else {
            ldk::ln::bolt11_payment::payment_parameters_from_invoice(&invoice)
                .map_err(|err| error::anyhow!("{:?}", err))?
        };
        self.channel_manager
            .manager()
            .send_payment(payment_hash, onion, payment_id, route, Retry::Attempts(10))
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
            .send_spontaneous_payment_with_retry(
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
