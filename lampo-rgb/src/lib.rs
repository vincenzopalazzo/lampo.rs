use std::time::Duration;
use lampo_common::ldk::ln::channelmanager::Retry;
use lampo_common::ldk::ln::channelmanager::PaymentId;
use lampo_common::error;
use lampod::LampoDaemon;
use lampod::jsonrpc::offchain::LampoVisitor;
use lampo_common::btc::bitcoin::hashes::Hash;
use lampo_common::ldk::invoice::payment;

struct RgbPayVisitor;

impl RgbPayVisitor {
    fn new() -> RgbPayVisitor { RgbPayVisitor }
}

impl LampoVisitor for RgbPayVisitor {
    fn pay_invoice(&self, ctx: &LampoDaemon, invoice_str: &str, amount_msat: Option<u64>) -> error::Result<()> {
        let invoice = ctx.offchain_manager().decode_invoice(invoice_str)?;
        let payment_id = PaymentId((*invoice.payment_hash()).into_inner());
        let channel_manager = ctx.channel_manager();
        let res = match payment::pay_invoice(
            &invoice,
            Retry::Timeout(Duration::from_secs(10)),
        ) {
            Ok(_payment_id) => {
                let payee_pubkey = invoice.recover_payee_pub_key();
                let amt_msat = invoice.amount_milli_satoshis().unwrap();
                log::info!(
                    "Sending {} msats to {}",
                    amt_msat,
                    payee_pubkey
                );
                Ok(())
            }
            Err(e) => error::bail!("Failed to send payment"),
        };
        Ok(())
    }
    
    fn pay_offer(&self, offer_str: &str, amount_msat: Option<u64>) -> error::Result<()> {
        unimplemented!("Bolt 12 is not supported for RGB")
    }
}
