use std::borrow::Borrow;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use lampo_common::bitcoin::hashes::sha256;
use lampo_common::bitcoin::hashes::Hash;
use lampo_common::chan;
use lampo_common::chrono::DateTime;
use lampo_common::chrono::Utc;
use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::error::Ok;
use lampo_common::event::Emitter;
use lampo_common::event::Event;
use lampo_common::event::Subscriber;
use lampo_common::handler::Handler;
use lampo_common::keys::LampoKeysManager;
use lampo_common::ldk::invoice::Bolt11Invoice;
use lampo_common::ldk::invoice::InvoiceBuilder;
use lampo_common::ldk::invoice::RouteHint;
use lampo_common::ldk::invoice::RouteHintHop;
use lampo_common::ldk::invoice::RoutingFees;
use lampo_common::ldk::ln::channelmanager::InterceptId;
use lampo_common::ldk::ln::channelmanager::MIN_FINAL_CLTV_EXPIRY_DELTA;
use lampo_common::ldk::ln::msgs::SocketAddress;
use lampo_common::ldk::ln::ChannelId;
use lampo_common::ldk::ln::PaymentHash;
use lampo_common::secp256k1::PublicKey;
use lampo_common::secp256k1::Secp256k1;

use lightning_liquidity::events::Event as liquidity_events;
use lightning_liquidity::lsps0::ser::RequestId;
use lightning_liquidity::lsps2::event::LSPS2ClientEvent;
use lightning_liquidity::lsps2::event::LSPS2ServiceEvent;
use lightning_liquidity::lsps2::msgs::OpeningFeeParams;
use lightning_liquidity::lsps2::msgs::RawOpeningFeeParams;
use lightning_liquidity::LiquidityManager;

use crate::chain::LampoChainManager;
use crate::ln::LampoChannel;
use crate::ln::LampoChannelManager;

use super::InnerLampoPeerManager;

pub type LampoLiquidity =
    LiquidityManager<Arc<LampoKeysManager>, Arc<LampoChannel>, Arc<LampoChainManager>>;

#[derive(Clone, Debug)]
pub struct LiquidityProvider {
    pub addr: SocketAddress,
    pub node_id: PublicKey,
    pub token: Option<String>,
    pub opening_params: Option<Vec<OpeningFeeParams>>,
    pub scid: Option<u64>,
    pub ctlv_exiry: Option<u32>,
}

#[derive(Clone)]
pub struct LampoLiquidityManager {
    lampo_liquidity: Arc<LampoLiquidity>,
    lampo_conf: LampoConf,
    // FIXME: How about Option<Arc<Mutex<LiquidityProvider>>>?
    lsp_provider: Arc<Mutex<Option<LiquidityProvider>>>,
    channel_manager: Arc<LampoChannelManager>,
    keys_manager: Arc<LampoKeysManager>,
    emitter: Emitter<Event>,
    subscriber: Subscriber<Event>,
}

// Maybe implement Emit event for this too?
impl LampoLiquidityManager {
    pub fn new_lsp(
        liquidity: Arc<LampoLiquidity>,
        conf: LampoConf,
        channel_manager: Arc<LampoChannelManager>,
        keys_manager: Arc<LampoKeysManager>,
    ) -> Self {
        let emitter = Emitter::default();
        let subscriber = emitter.subscriber();
        Self {
            lampo_liquidity: liquidity,
            lampo_conf: conf,
            lsp_provider: Arc::new(Mutex::new(None)),
            channel_manager,
            keys_manager,
            emitter,
            subscriber,
        }
    }

    // This should not be initiated when we open our node only when we
    // provide some args inside cli or in lampo.conf

    pub fn has_some_provider(&self) -> Option<LiquidityProvider> {
        self.lsp_provider.lock().unwrap().clone()
    }
    pub fn configure_as_liquidity_consumer(
        &mut self,
        node_id: PublicKey,
        addr: SocketAddress,
        token: Option<String>,
    ) -> error::Result<()> {
        log::info!("Starting lampo-liquidity manager as a consumer!");
        if self.has_some_provider().is_none() {
            println!("We are inside hqllo 1");
            let liquidity_provider = LiquidityProvider {
                addr,
                node_id,
                token,
                opening_params: None,
                scid: None,
                ctlv_exiry: None,
            };

            *self.lsp_provider.lock().unwrap() = Some(liquidity_provider);
            let res = self.lsp_provider.lock().unwrap().clone();
            log::info!("This is the result liq prov: {:?}", res);
        }

        println!(
            "This is the object we are having after configure as liquidity consumer: {:?}",
            self.lsp_provider.clone()
        );

        Ok(())
    }

    pub fn liquidity_manager(&self) -> Arc<LampoLiquidity> {
        self.lampo_liquidity.clone()
    }

    pub fn htlc_intercepted(
        &self,
        intercept_scid: u64,
        intercept_id: InterceptId,
        expected_outbound_amount_msat: u64,
        payment_hash: PaymentHash,
    ) -> error::Result<()> {
        self.liquidity_manager()
            .lsps2_service_handler()
            .unwrap()
            .htlc_intercepted(
                intercept_scid,
                intercept_id,
                expected_outbound_amount_msat,
                payment_hash,
            )
            .map_err(|e| error::anyhow!("Error : {:?}", e))?;

        Ok(())
    }

    pub fn channel_ready(
        &self,
        user_channel_id: u128,
        channel_id: &ChannelId,
        counterparty_node_id: &PublicKey,
    ) -> error::Result<()> {
        self.lampo_liquidity
            .lsps2_service_handler()
            .unwrap()
            .channel_ready(user_channel_id, channel_id, counterparty_node_id)
            .map_err(|e| error::anyhow!("Error occured : {:?}", e))?;

        Ok(())
    }

    pub fn get_events(&self) -> Vec<liquidity_events> {
        self.lampo_liquidity.get_and_clear_pending_events()
    }

    pub fn set_peer_manager(&self, peer_manager: Arc<InnerLampoPeerManager>) {
        let process_msgs_callback = move || peer_manager.process_events();
        self.liquidity_manager()
            .set_process_msgs_callback(process_msgs_callback);
    }

    // getinfo(server) -> OpeningParamsReady(client) -> BuyRequest(client) -> InvoiceParametersReady(client) -> OpenChannel(server)
    pub async fn listen(&mut self) -> error::Result<()> {
        log::info!("GOT AN EVENT!!!");
        match self.lampo_liquidity.next_event_async().await {
            liquidity_events::LSPS0Client(..) => unimplemented!(),
            liquidity_events::LSPS2Client(LSPS2ClientEvent::OpeningParametersReady {
                counterparty_node_id,
                opening_fee_params_menu,
                ..
            }) => {
                log::info!("Received a opening_params ready event!");
                // let res = self.get_lsp_provider().node_id;
                if &self.get_lsp_provider().node_id != &counterparty_node_id {
                    error::bail!("Recieved Unknown OpeningParametersReady event");
                }

                // TODO: Handle this in a better way as we can get new opening_params from a
                // LSP if it fails to responds within a certain time
                if self.get_lsp_provider().opening_params.is_some() {
                    error::bail!("We already have some params inside lsp_provider");
                }

                self.lsp_provider
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap()
                    .opening_params = Some(opening_fee_params_menu);
                self.emit(Event::Liquidity(
                    "Got a openingparamsready event".to_string(),
                ));
                Ok(())
            }
            liquidity_events::LSPS2Client(LSPS2ClientEvent::InvoiceParametersReady {
                counterparty_node_id,
                intercept_scid,
                cltv_expiry_delta,
                ..
            }) => {
                if counterparty_node_id != self.get_lsp_provider().node_id {
                    error::bail!("Unknown lsp");
                }

                // We will take the intercept_scid and cltv_expiry_delta from here and
                // generate an invoice from these params
                self.get_lsp_provider().ctlv_exiry = Some(cltv_expiry_delta);
                self.get_lsp_provider().scid = Some(intercept_scid);
                self.emit(Event::Liquidity(
                    "Got a invoiceparamsReady event".to_string(),
                ));

                Ok(())
            }
            liquidity_events::LSPS2Service(LSPS2ServiceEvent::GetInfo {
                request_id,
                counterparty_node_id,
                token,
            }) => {
                log::info!("Received a getinfo request!");
                let service_handler = self.lampo_liquidity.lsps2_service_handler().unwrap();

                let min_fee_msat = 0;
                let proportional = 0;
                let mut valid_until: DateTime<Utc> = Utc::now();
                valid_until += Duration::from_secs_f64(600_f64);
                let min_lifetime = 1008;
                let max_client_to_self_delay = 144;
                let min_payment_size_msat = 1000;
                let max_payment_size_msat = 10_000_000_000;

                let opening_fee_params = RawOpeningFeeParams {
                    min_fee_msat,
                    proportional,
                    valid_until,
                    min_lifetime,
                    max_client_to_self_delay,
                    min_payment_size_msat,
                    max_payment_size_msat,
                };

                let opening_fee_params_menu = vec![opening_fee_params];

                service_handler
                    .opening_fee_params_generated(
                        &counterparty_node_id,
                        request_id,
                        opening_fee_params_menu,
                    )
                    .map_err(|e| error::anyhow!("Error : {:?}", e))?;

                Ok(())
            }
            liquidity_events::LSPS2Service(LSPS2ServiceEvent::BuyRequest {
                request_id,
                counterparty_node_id,
                opening_fee_params: _,
                payment_size_msat: _,
            }) => {
                let user_channel_id = 0;
                let scid = self
                    .channel_manager
                    .channeld
                    .as_ref()
                    .unwrap()
                    .get_intercept_scid();
                let cltv_expiry_delta = 72;
                let client_trusts_lsp = true;

                let lsps2_service_handler = self.lampo_liquidity.lsps2_service_handler().unwrap();

                lsps2_service_handler
                    .invoice_parameters_generated(
                        &counterparty_node_id,
                        request_id,
                        scid,
                        cltv_expiry_delta,
                        client_trusts_lsp,
                        user_channel_id,
                    )
                    .map_err(|e| error::anyhow!("Error occured: {:?}", e))?;

                Ok(())
            }
            liquidity_events::LSPS2Service(LSPS2ServiceEvent::OpenChannel {
                their_network_key,
                amt_to_forward_msat,
                opening_fee_msat,
                user_channel_id,
                intercept_scid,
            }) => {
                let channel_size_sats = (amt_to_forward_msat / 1000) * 4;
                let mut config = self.lampo_conf.ldk_conf;
                config
                    .channel_handshake_config
                    .max_inbound_htlc_value_in_flight_percent_of_channel = 100;
                config.channel_config.forwarding_fee_base_msat = 0;
                config.channel_config.forwarding_fee_proportional_millionths = 0;

                // TODO(Harshit): Make a different function to get channeld
                self.channel_manager
                    .channeld
                    .as_ref()
                    .unwrap()
                    .create_channel(
                        their_network_key,
                        channel_size_sats,
                        0,
                        user_channel_id,
                        None,
                        Some(config),
                    )
                    .map_err(|e| error::anyhow!("Error occured: {:?}", e))?;

                Ok(())
            }
        }
    }

    fn client_request_opening_params(&self) -> error::Result<RequestId> {
        let provider = self.has_some_provider().clone();
        println!("This is the object before is_none call : {:?}", provider);
        if provider.is_none() {
            error::bail!("LSP provider not configured")
        }

        let node_id = provider.clone().unwrap().node_id;
        let token = provider.unwrap().token;
        let res = self
            .lampo_liquidity
            .lsps2_client_handler()
            .unwrap()
            .request_opening_params(node_id, token);

        log::info!("This is the request_id: {:?}", res);

        loop {
            let event = self.events().recv_timeout(Duration::from_secs(30)).unwrap();
            if let Event::Liquidity(str) = event {
                log::info!("We got an liquidity event!");
                return Ok(res);
            } else {
                error::bail!("Wrong event received")
            }
        }

        Ok(res)
    }

    // Select the best fee_param from a list of fee_param given by the lsp provider
    // and then forward the request to the LSP for invoice generation
    // This will respond in InvoiceParametersReady event
    fn buy_request(&self, best_fee_param: OpeningFeeParams, amount_msat: u64) -> error::Result<()> {
        let node_id = self.get_lsp_provider().node_id;
        self.lampo_liquidity
            .lsps2_client_handler()
            .unwrap()
            .select_opening_params(node_id, Some(amount_msat), best_fee_param)
            .map_err(|err| error::anyhow!("Error Occured : {:?}", err))?;

        let _ = tokio::time::sleep(Duration::from_secs(10));
        Ok(())
    }

    pub fn create_a_jit_channel(
        &self,
        amount_msat: u64,
        description: String,
    ) -> error::Result<Bolt11Invoice> {
        self.client_request_opening_params()?;
        let fee_param = self.get_lsp_provider().opening_params.clone();
        if fee_param.is_none() {
            error::bail!("At this point best_fee_param should not be None");
        }

        // TODO: We need to provide a suitable algorithm to get the best_params from all the
        // opening params that we get from the peer. For now we are getting the first param
        let best_fee_param = &fee_param.unwrap().clone()[0];

        self.buy_request(best_fee_param.clone(), amount_msat)?;
        let invoice = self.generate_invoice_for_jit_channel(amount_msat, description)?;

        Ok(invoice)
    }

    pub fn get_lsp_provider(&self) -> LiquidityProvider {
        let res = self.lsp_provider.lock().unwrap().clone();
        println!("This is the get_lsp_provider: {:?}", res);
        self.lsp_provider
            .lock()
            .unwrap()
            .clone()
            .unwrap()
            .borrow()
            .clone()
    }

    fn generate_invoice_for_jit_channel(
        &self,
        amount_msat: u64,
        description: String,
    ) -> error::Result<Bolt11Invoice> {
        let scid = self.get_lsp_provider().scid.unwrap();
        let cltv = self.get_lsp_provider().ctlv_exiry.unwrap();
        let node_id = self.get_lsp_provider().node_id;

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

impl Handler for LampoLiquidityManager {
    fn emit(&self, event: Event) {
        log::debug!(target: "emitter", "emit event: {:?}", event);
        self.emitter.emit(event)
    }

    fn events(&self) -> chan::Receiver<Event> {
        log::debug!(target: "listener", "subscribe for events");
        self.subscriber.subscribe()
    }
}
