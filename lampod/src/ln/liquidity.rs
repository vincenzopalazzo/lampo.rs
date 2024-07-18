use std::sync::Arc;
use std::time::Duration;

use lampo_common::bitcoin::hashes::sha256;
use lampo_common::bitcoin::hashes::Hash;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::keys::LampoKeysManager;
use lampo_common::ldk::invoice::Bolt11Invoice;
use lampo_common::ldk::invoice::InvoiceBuilder;
use lampo_common::ldk::invoice::RouteHint;
use lampo_common::ldk::invoice::RouteHintHop;
use lampo_common::ldk::invoice::RoutingFees;
use lampo_common::ldk::ln::channelmanager::MIN_FINAL_CLTV_EXPIRY_DELTA;
use lampo_common::ldk::ln::msgs::SocketAddress;
use lampo_common::secp256k1::PublicKey;
use lampo_common::secp256k1::Secp256k1;

use lightning_liquidity::events::Event;
use lightning_liquidity::lsps0::ser::RequestId;
use lightning_liquidity::lsps2::event::LSPS2ClientEvent;
use lightning_liquidity::lsps2::event::LSPS2ServiceEvent;
use lightning_liquidity::lsps2::msgs::OpeningFeeParams;
use lightning_liquidity::LiquidityManager;

use crate::chain::LampoChainManager;
use crate::ln::LampoChannel;
use crate::ln::LampoChannelManager;
use crate::LampoDaemon;

pub type LampoLiquidity =
    LiquidityManager<Arc<LampoKeysManager>, Arc<LampoChannel>, Arc<LampoChainManager>>;

// Configured when we are acting as a client sourcing lsp from
// a different node, here provider is someone who is providing us liquidity.
pub struct LiquidityProvider {
    pub addr: SocketAddress,
    pub node_id: PublicKey,
    pub token: Option<String>,
    pub opening_params: Option<Vec<OpeningFeeParams>>,
    pub scid: Option<u64>,
    pub ctlv_exiry: Option<u32>,
}

pub struct LampoLiquidityManager {
    lampo_liquidity: Arc<LampoLiquidity>,
    lampo_conf: LampoConf,
    // TODO: We can't use Arc here as we are modifying data. Fix this.
    lsp_provider: Option<LiquidityProvider>,
    channel_manager: Arc<LampoChannelManager>,
    keys_manager: Arc<LampoKeysManager>,
}

impl LampoLiquidityManager {
    pub fn new(
        liquidity: Arc<LampoLiquidity>,
        conf: LampoConf,
        provider: Option<LiquidityProvider>,
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
        conf: LampoConf,
        channel_manager: Arc<LampoChannelManager>,
        keys_manager: Arc<LampoKeysManager>,
        address: SocketAddress,
        node_id: PublicKey,
        token: Option<String>,
    ) -> Self {
        // FIXME: Implement a new function for this
        let lsp_provider = LiquidityProvider {
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

    pub fn get_events(&self) -> Vec<Event> {
        self.lampo_liquidity.get_and_clear_pending_events()
    }

    pub async fn listen(&mut self) -> error::Result<()> {
        match self.lampo_liquidity.next_event_async().await {
            Event::LSPS0Client(..) => todo!(),
            Event::LSPS2Client(LSPS2ClientEvent::OpeningParametersReady {
                counterparty_node_id,
                opening_fee_params_menu,
                ..
            }) => {
                if &self.lsp_provider.as_ref().unwrap().node_id != &counterparty_node_id {
                    error::bail!("Recieved Unknown OpeningParametersReady event");
                }

                // TODO: Handle this in a better way as we can get new opening_params from a
                // LSP if it fails to responds within a certain time
                if self.lsp_provider.as_ref().unwrap().opening_params.is_some() {
                    error::bail!("We already have some params inside lsp_provider");
                }

                self.lsp_provider.as_mut().unwrap().opening_params = Some(opening_fee_params_menu);
                Ok(())
            }
            Event::LSPS2Client(LSPS2ClientEvent::InvoiceParametersReady {
                counterparty_node_id,
                intercept_scid,
                cltv_expiry_delta,
                ..
            }) => {
                if counterparty_node_id != self.lsp_provider.as_mut().unwrap().node_id {
                    error::bail!("Unknown lsp");
                }

                // We will take the intercept_scid and cltv_expiry_delta from here and
                // generate an invoice from these params
                self.lsp_provider.as_mut().unwrap().ctlv_exiry = Some(cltv_expiry_delta);
                self.lsp_provider.as_mut().unwrap().scid = Some(intercept_scid);

                Ok(())
            }
            Event::LSPS2Service(LSPS2ServiceEvent::BuyRequest {
                request_id,
                counterparty_node_id,
                opening_fee_params,
                payment_size_msat,
            }) => todo!(),
            Event::LSPS2Service(LSPS2ServiceEvent::GetInfo {
                request_id,
                counterparty_node_id,
                token,
            }) => todo!(),
            Event::LSPS2Service(LSPS2ServiceEvent::OpenChannel {
                their_network_key,
                amt_to_forward_msat,
                opening_fee_msat,
                user_channel_id,
                intercept_scid,
            }) => todo!(),
        }
    }

    async fn client_request_opening_params(&self) -> error::Result<RequestId> {
        let provider = self.lsp_provider.as_ref();
        if provider.is_none() {
            error::bail!("LSP provider not configured")
        }

        let node_id = provider.unwrap().node_id;
        let token = provider.unwrap().token.clone();
        let res = self
            .lampo_liquidity
            .lsps2_client_handler()
            .unwrap()
            .request_opening_params(node_id, token);

        tokio::time::sleep(Duration::from_secs(10)).await;

        Ok(res)
    }

    // Select the best fee_param from a list of fee_param given by the lsp provider
    // and then forward the request to the LSP for invoice generation
    // This will respond in InvoiceParametersReady event
    async fn buy_request(
        &self,
        best_fee_param: OpeningFeeParams,
        amount_msat: u64,
    ) -> error::Result<()> {
        let node_id = self.lsp_provider.as_ref().unwrap().node_id;
        self.lampo_liquidity
            .lsps2_client_handler()
            .unwrap()
            .select_opening_params(node_id, Some(amount_msat), best_fee_param)
            .map_err(|err| error::anyhow!("Error Occured : {:?}", err))?;

        tokio::time::sleep(Duration::from_secs(10)).await;
        Ok(())
    }

    pub async fn create_a_jit_channel(
        &self,
        amount_msat: u64,
        description: String,
    ) -> error::Result<Bolt11Invoice> {
        // TODO: We also need to connect to the client
        self.client_request_opening_params().await?;
        let fee_param = self.lsp_provider.as_ref().unwrap().opening_params.clone();
        if fee_param.is_none() {
            error::bail!("At this point best_fee_param should not be None");
        }

        // TODO: We need to provide a suitable algorithm to get the best_params from all the
        // opening params that we get from the peer. For now we are getting the first param
        let best_fee_param = &fee_param.unwrap().clone()[0];

        self.buy_request(best_fee_param.clone(), amount_msat)
            .await?;
        let invoice = self.generate_invoice_for_jit_channel(amount_msat, description)?;

        Ok(invoice)
    }

    fn generate_invoice_for_jit_channel(
        &self,
        amount_msat: u64,
        description: String,
    ) -> error::Result<Bolt11Invoice> {
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

        let payment_hash = sha256::Hash::from_slice(&paymen_hash.0)?;

        let currency = self.lampo_conf.network.into();
        let mut invoice_builder = InvoiceBuilder::new(currency)
            .description(description)
            .payment_hash(payment_hash)
            .payment_secret(payment_secret)
            .current_timestamp()
            .min_final_cltv_expiry_delta(min_final_cltv_expiry_delta.into())
            .expiry_time(Duration::from_secs(expiry_seconds.into()))
            .private_route(route_hint);

        invoice_builder = invoice_builder
            .amount_milli_satoshis(amount_msat)
            .basic_mpp();

        let invoice = invoice_builder.build_signed(|hash| {
            Secp256k1::new().sign_ecdsa_recoverable(hash, &self.keys_manager.get_node_secret_key())
        })?;

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
            lampo_conf: value.conf.clone(),
            lsp_provider: None,
            channel_manager: value.channel_manager(),
            keys_manager: value.offchain_manager().key_manager(),
        }
    }
}
