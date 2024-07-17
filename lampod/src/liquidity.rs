use std::{sync::Arc, time::Duration};

use lampo_common::{
    bitcoin::hashes::{sha256, Hash},
    conf::LampoConf,
    error,
    keys::LampoKeysManager,
    ldk::{
        invoice::{Bolt11Invoice, InvoiceBuilder, RouteHint, RouteHintHop, RoutingFees},
        ln::{channelmanager::MIN_FINAL_CLTV_EXPIRY_DELTA, msgs::SocketAddress},
        sign::KeysManager,
    },
    secp256k1::{PublicKey, Secp256k1},
};
use lightning_liquidity::{
    events::Event,
    lsps0::ser::RequestId,
    lsps2::{event::LSPS2ClientEvent, msgs::OpeningFeeParams},
    LiquidityManager,
};

use crate::{
    chain::LampoChainManager,
    ln::{LampoChannel, LampoChannelManager},
    LampoDaemon,
};

pub type LampoLiquidity =
    LiquidityManager<Arc<LampoKeysManager>, Arc<LampoChannel>, Arc<LampoChainManager>>;

// Configured when we are acting as a client sourcing lsp from
// a different node, here provider is someone who is providing us liquidity.
pub struct LSPProvider {
    pub addr: SocketAddress,
    pub node_id: PublicKey,
    pub token: Option<String>,
    // This will be initialised when we get the response of get_info request
    pub opening_params: Option<Vec<OpeningFeeParams>>,
    // The short channel_id that will be sent by the LSP required for generating invoice
    pub scid: Option<u64>,
    // Also required while creating invoice
    pub ctlv_exiry: Option<u32>,
}

pub struct LampoLiquidityManager {
    // lampod: Arc<LampoDaemon>,
    lampo_liquidity: Arc<LampoLiquidity>,
    lampo_conf: Arc<LampoConf>,
    // TODO: We can't use Arc here as we are modifying data. Fix this.
    lsp_provider: Option<LSPProvider>,
    channel_manager: Arc<LampoChannelManager>,
    keys_manager: Arc<LampoKeysManager>,
}

impl LampoLiquidityManager {
    pub fn new(
        liquidity: Arc<LampoLiquidity>,
        conf: Arc<LampoConf>,
        provider: Option<LSPProvider>,
        channel_manager: Arc<LampoChannelManager>,
        keys_manager: Arc<LampoKeysManager>,
    ) -> Self {
        Self {
            lampo_liquidity: liquidity,
            lampo_conf: conf,
            lsp_provider: provider,
            channel_manager,
            keys_manager,
        }
    }

    // Behaving as a client
    pub fn new_liquidity_consumer(
        liquidity: Arc<LampoLiquidity>,
        conf: Arc<LampoConf>,
        channel_manager: Arc<LampoChannelManager>,
        keys_manager: Arc<LampoKeysManager>,
        address: SocketAddress,
        node_id: PublicKey,
        token: Option<String>,
    ) -> Self {
        // FIXME: Implement a new function for this
        let lsp_provider = LSPProvider {
            addr: address,
            node_id,
            token,
            opening_params: None,
            ctlv_exiry: None,
            scid: None,
        };
        Self {
            lampo_liquidity: liquidity,
            lampo_conf: conf,
            channel_manager,
            lsp_provider: Some(lsp_provider),
            keys_manager,
        }
    }

    pub fn get_events(&self) {
        self.lampo_liquidity.get_and_clear_pending_events();
    }

    pub async fn listen(&mut self) {
        match self.lampo_liquidity.next_event_async().await {
            Event::LSPS0Client(..) => todo!(),
            Event::LSPS2Client(LSPS2ClientEvent::OpeningParametersReady {
                request_id,
                counterparty_node_id,
                opening_fee_params_menu,
            }) => {
                // This event is called when the LSP server provides us with
                // all the OpeningChannelParams in response of get_info request
                if &self.lsp_provider.as_ref().unwrap().node_id != &counterparty_node_id {
                    log::info!("Recieved Unknown OpeningParametersReady event")
                }

                // TODO: Handle this in a better way as we can get new opening_params from a
                // LSP if it fails to responds within a certain time
                if self.lsp_provider.as_ref().unwrap().opening_params.is_some() {
                    log::info!("We already have some params inside lsp_provider")
                }

                self.lsp_provider.as_mut().unwrap().opening_params = Some(opening_fee_params_menu)
            }
            Event::LSPS2Client(LSPS2ClientEvent::InvoiceParametersReady {
                request_id,
                counterparty_node_id,
                intercept_scid,
                cltv_expiry_delta,
                payment_size_msat,
            }) => {
                // TODO: Check if the counerparty_node_id is equal to the one that we
                // currently have.

                // We will take the intercept_scid and cltv_expiry_delta from here and
                // generate an invoice from these params
                self.lsp_provider.as_mut().unwrap().ctlv_exiry = Some(cltv_expiry_delta);
                self.lsp_provider.as_mut().unwrap().scid = Some(intercept_scid);
                todo!()
            }
            Event::LSPS2Service(_) => todo!(),
        }
    }

    // Acting as a client
    // This function will be used to open a channel by using other LSPs liquidity.
    fn client_request_opening_params(&self) -> error::Result<RequestId> {
        let provider = self.lsp_provider.as_ref();
        if provider.is_none() {
            error::bail!("LSP provider not configured")
        }

        let node_id = provider.unwrap().node_id;
        let token = provider.unwrap().token.clone();
        // self.lampo_liquidity.lsps2_client_handler().unwrap().select_opening_params(counterparty_node_id, payment_size_msat, opening_fee_params)
        let res = self
            .lampo_liquidity
            .lsps2_client_handler()
            .unwrap()
            .request_opening_params(node_id, token);

        // TOODO: Check if there is no response after we call this function within a
        // certain time (See LSP2)

        Ok(res)
    }

    // Select the best fee_param from a list of fee_param given by the lsp provider
    // and then forward the request to the LSP for invoice generation
    // This will respond in InvoiceParametersReady event
    pub fn buy_request(
        &self,
        best_fee_param: OpeningFeeParams,
        amount_msat: u64,
    ) -> error::Result<()> {
        let node_id = self.lsp_provider.as_ref().unwrap().node_id;
        let res = self
            .lampo_liquidity
            .lsps2_client_handler()
            .unwrap()
            .select_opening_params(node_id, Some(amount_msat), best_fee_param);

        Ok(())
    }

    pub fn create_a_jit_channel(&self) {
        todo!()
    }

    // Scid is returned from a lsp provider, then give this invoice to the payer that is wanting to pay this invoice.
    // lsp_node_id would act as a route_hint from where the payment should have a hop.
    fn generate_invoice_for_jit_channel(&self) -> error::Result<Bolt11Invoice> {
        let scid = self.lsp_provider.as_ref().unwrap().scid.unwrap();
        let cltv = self.lsp_provider.as_ref().unwrap().ctlv_exiry.unwrap();
        let node_id = self.lsp_provider.as_ref().unwrap().node_id;

        // TODO: This needs to be configurable
        let expiry_seconds = 5;

        let min_final_cltv_expiry_delta = MIN_FINAL_CLTV_EXPIRY_DELTA + 2;

        let res = self
            .channel_manager
            .channeld
            .clone()
            .unwrap()
            .create_inbound_payment(None, expiry_seconds, Some(min_final_cltv_expiry_delta));

        let paymen_hash = res.unwrap().0;
        let payment_secret = res.unwrap().1;

        let route_hint = RouteHint(vec![RouteHintHop {
            src_node_id: node_id,
            short_channel_id: scid,
            fees: RoutingFees {
                base_msat: 0,
                proportional_millionths: 0,
            },
            cltv_expiry_delta: cltv as u16,
            htlc_minimum_msat: None,
            htlc_maximum_msat: None,
        }]);

        let payment_hash = sha256::Hash::from_slice(&paymen_hash.0).unwrap();

        let currency = self.lampo_conf.network.into();
        // TODO: Description should be configurable
        let mut invoice_builder = InvoiceBuilder::new(currency)
            .description("LSP".to_string())
            .payment_hash(payment_hash)
            .payment_secret(payment_secret)
            .current_timestamp()
            .min_final_cltv_expiry_delta(min_final_cltv_expiry_delta.into())
            .expiry_time(Duration::from_secs(expiry_seconds.into()))
            .private_route(route_hint);

        //FIXME: Make the invoice amount configurable
        invoice_builder = invoice_builder
            .amount_milli_satoshis(100_000_000)
            .basic_mpp();

        let invoice = invoice_builder
            .build_signed(|hash| {
                Secp256k1::new()
                    .sign_ecdsa_recoverable(hash, &self.keys_manager.get_node_secret_key())
            })
            .unwrap();

        Ok(invoice)
    }
}

impl From<Arc<LampoDaemon>> for LampoLiquidityManager {
    fn from(value: Arc<LampoDaemon>) -> Self {
        Self {
            lampo_liquidity: Arc::new(LampoLiquidity::new(
                value.offchain_manager().key_manager(),
                value.channel_manager().manager(),
                Some(value.onchain_manager()),
                None,
                None,
                None,
            )),
            lampo_conf: Arc::new(value.conf.clone()),
            lsp_provider: None,
            channel_manager: value.channel_manager(),
            keys_manager: value.offchain_manager().key_manager(),
        }
    }
}
