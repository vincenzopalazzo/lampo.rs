use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use lampo_common::bitcoin::hashes::sha256;
use lampo_common::bitcoin::hashes::Hash;
use lampo_common::chrono::DateTime;
use lampo_common::chrono::Utc;
use lampo_common::conf::LampoConf;
use lampo_common::conf::Liquidity;
use lampo_common::error;
use lampo_common::error::Ok;
use lampo_common::event::liquidity::LiquidityEvent;
use lampo_common::event::Event;
use lampo_common::handler::Handler;
use lampo_common::keys::LampoKeysManager;
use lampo_common::ldk::events::HTLCDestination;
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
use lightning_liquidity::lsps2::event::LSPS2ClientEvent;
use lightning_liquidity::lsps2::event::LSPS2ServiceEvent;
use lightning_liquidity::lsps2::msgs::OpeningFeeParams;
use lightning_liquidity::lsps2::msgs::RawOpeningFeeParams;
use lightning_liquidity::LiquidityManager;

use crate::actions::handler::LampoHandler;
use crate::chain::LampoChainManager;
use crate::ln::LampoChannel;
use crate::ln::LampoChannelManager;

use super::InnerLampoPeerManager;

pub type LampoLiquidity =
    LiquidityManager<Arc<LampoKeysManager>, Arc<LampoChannel>, Arc<LampoChainManager>>;

#[derive(Clone)]
struct LSPManager {
    pub addr: SocketAddress,
    pub node_id: PublicKey,
    pub token: Option<String>,
}

impl LSPManager {
    pub fn new(addr: SocketAddress, node_id: PublicKey, token: Option<String>) -> Self {
        LSPManager {
            addr,
            node_id,
            token,
        }
    }
}

pub struct LampoLiquidityManager {
    lampo_liquidity: Arc<LampoLiquidity>,
    lampo_conf: LampoConf,
    // FIXME: Change this name
    lsp_manager: Option<LSPManager>,
    channel_manager: Arc<LampoChannelManager>,
    keys_manager: Arc<LampoKeysManager>,
    handler: Mutex<Option<Arc<LampoHandler>>>,
}

impl LampoLiquidityManager {
    pub fn new_lsp(
        liquidity: Arc<LampoLiquidity>,
        conf: LampoConf,
        channel_manager: Arc<LampoChannelManager>,
        keys_manager: Arc<LampoKeysManager>,
    ) -> Self {
        Self {
            lampo_liquidity: liquidity,
            lampo_conf: conf,
            lsp_manager: None,
            channel_manager,
            keys_manager,
            handler: Mutex::new(None),
        }
    }

    pub fn new_lsp_as_client(
        liquidity: Arc<LampoLiquidity>,
        conf: LampoConf,
        channel_manager: Arc<LampoChannelManager>,
        keys_manager: Arc<LampoKeysManager>,
        node_id: PublicKey,
        socket_addr: SocketAddress,
    ) -> Self {
        let lsp = LSPManager::new(socket_addr, node_id, None);
        Self {
            lampo_liquidity: liquidity,
            lampo_conf: conf,
            lsp_manager: Some(lsp),
            channel_manager,
            keys_manager,
            handler: Mutex::new(None),
        }
    }

    pub fn htlc_handling_failed(
        &self,
        failed_next_destination: HTLCDestination,
    ) -> error::Result<()> {
        match self.lampo_conf.liquidity.clone().unwrap() {
            Liquidity::Provider => {
                self.liquidity_manager()
                    .lsps2_service_handler()
                    .unwrap()
                    .htlc_handling_failed(failed_next_destination)
                    .map_err(|e| error::anyhow!("Error : {:?}", e))?;
            }
            _ => return Ok(()),
        }

        Ok(())
    }

    fn has_some_provider(&self) -> Option<LSPManager> {
        self.lsp_manager.clone()
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

    pub fn payment_forwarded(&self, next_channel_id: ChannelId) -> error::Result<()> {
        self.liquidity_manager()
            .lsps2_service_handler()
            .unwrap()
            .payment_forwarded(next_channel_id)
            .map_err(|e| error::anyhow!("Error : {:?}", e))?;
        Ok(())
    }

    pub fn channel_ready(
        &self,
        user_channel_id: u128,
        channel_id: &ChannelId,
        counterparty_node_id: &PublicKey,
    ) -> error::Result<()> {
        // Here unwrap is failing
        match self.lampo_conf.liquidity.clone().unwrap() {
            Liquidity::Provider => {
                self.lampo_liquidity
                    .lsps2_service_handler()
                    .unwrap()
                    .channel_ready(user_channel_id, channel_id, counterparty_node_id)
                    .map_err(|e| error::anyhow!("Error occured : {:?}", e))?;
            }
            _ => return Ok(()),
        }

        Ok(())
    }

    pub fn set_peer_manager(&self, peer_manager: Arc<InnerLampoPeerManager>) {
        let process_msgs_callback = move || peer_manager.process_events();
        self.liquidity_manager()
            .set_process_msgs_callback(process_msgs_callback);
    }

    pub fn set_handler(&self, handler: Arc<LampoHandler>) {
        *self.handler.lock().unwrap() = Some(handler);
    }

    // getinfo(server) -> OpeningParamsReady(client) -> BuyRequest(client) -> InvoiceParametersReady(client) -> OpenChannel(server)
    pub async fn listen(&self) -> error::Result<()> {
        match self.lampo_liquidity.next_event_async().await {
            // Get the opening_fee_params_menu from here and should be inside the event emitted.
            liquidity_events::LSPS2Client(LSPS2ClientEvent::OpeningParametersReady {
                counterparty_node_id,
                opening_fee_params_menu,
                ..
            }) => {
                log::info!(
                    "Received a opening_params ready event: {:?}",
                    opening_fee_params_menu
                );
                if &self.get_lsp_provider().node_id != &counterparty_node_id {
                    error::bail!("Recieved Unknown OpeningParametersReady event");
                }
                self.handler()
                    .emit(Event::Liquidity(LiquidityEvent::OpenParamsReady {
                        counterparty_node_id,
                        opening_fee_params_menu,
                    }));

                Ok(())
            }
            liquidity_events::LSPS2Client(LSPS2ClientEvent::InvoiceParametersReady {
                counterparty_node_id,
                intercept_scid,
                cltv_expiry_delta,
                ..
            }) => {
                log::info!("Received a invoice params ready event");
                if counterparty_node_id != self.get_lsp_provider().node_id {
                    error::bail!("Unknown lsp");
                }
                self.handler()
                    .emit(Event::Liquidity(LiquidityEvent::InvoiceparamsReady {
                        counterparty_node_id,
                        intercept_scid,
                        cltv_expiry_delta,
                    }));
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
                        request_id.clone(),
                        opening_fee_params_menu,
                    )
                    .map_err(|e| error::anyhow!("Error : {:?}", e))?;

                self.handler()
                    .emit(Event::Liquidity(LiquidityEvent::Geinfo {
                        request_id,
                        counterparty_node_id,
                        token,
                    }));

                Ok(())
            }
            liquidity_events::LSPS2Service(LSPS2ServiceEvent::BuyRequest {
                request_id,
                counterparty_node_id,
                opening_fee_params,
                payment_size_msat,
            }) => {
                log::info!("Buy Request event received!");
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
                        request_id.clone(),
                        scid,
                        cltv_expiry_delta,
                        client_trusts_lsp,
                        user_channel_id,
                    )
                    .map_err(|e| error::anyhow!("Error occured: {:?}", e))?;

                self.handler()
                    .emit(Event::Liquidity(LiquidityEvent::BuyRequest {
                        request_id,
                        counterparty_node_id,
                        opening_fee_params,
                        payment_size_msat,
                    }));

                Ok(())
            }
            liquidity_events::LSPS2Service(LSPS2ServiceEvent::OpenChannel {
                their_network_key,
                amt_to_forward_msat,
                opening_fee_msat,
                user_channel_id,
                intercept_scid,
            }) => {
                log::info!("Open Channel request received");
                let channel_size_sats = (amt_to_forward_msat / 1000) * 4;
                let mut config = self.lampo_conf.ldk_conf;
                config
                    .channel_handshake_config
                    .max_inbound_htlc_value_in_flight_percent_of_channel = 100;
                config.channel_config.forwarding_fee_base_msat = 0;
                config.channel_config.forwarding_fee_proportional_millionths = 0;

                let res = self
                    .channel_manager
                    .channeld()
                    .create_channel(
                        their_network_key,
                        channel_size_sats,
                        0,
                        user_channel_id,
                        None,
                        Some(config),
                    )
                    .map_err(|e| error::anyhow!("Error occured: {:?}", e))?;

                self.handler()
                    .emit(Event::Liquidity(LiquidityEvent::OpenChannel {
                        their_network_key,
                        amt_to_forward_msat,
                        opening_fee_msat,
                        user_channel_id,
                        intercept_scid,
                    }));

                Ok(())
            }
            _ => error::bail!("Wrong event received"),
        }
    }

    fn client_request_opening_params(&self) -> error::Result<Vec<OpeningFeeParams>> {
        let provider = self.has_some_provider().clone();
        if provider.is_none() {
            error::bail!("LSP provider not configured")
        }
        let node_id = self.lsp_manager.clone().unwrap().node_id;
        let token = None;
        let res = self
            .lampo_liquidity
            .lsps2_client_handler()
            .unwrap()
            .request_opening_params(node_id, token);

        log::info!("This is the request_id: {:?}", res);

        let result = loop {
            let events = self.handler().events();
            let event = events.recv_timeout(std::time::Duration::from_secs(30))?;

            if let Event::Liquidity(LiquidityEvent::OpenParamsReady {
                counterparty_node_id,
                opening_fee_params_menu,
            }) = event
            {
                break Some(opening_fee_params_menu);
            }
        };

        Ok(result.unwrap())
    }

    // Select the best fee_param from a list of fee_param given by the lsp provider
    // and then forward the request to the LSP for invoice generation
    // This will respond in InvoiceParametersReady event
    fn buy_request(
        &self,
        best_fee_param: OpeningFeeParams,
        amount_msat: u64,
    ) -> error::Result<(u64, u32)> {
        let node_id = self.get_lsp_provider().node_id;
        self.lampo_liquidity
            .lsps2_client_handler()
            .unwrap()
            .select_opening_params(node_id, Some(amount_msat), best_fee_param)
            .map_err(|err| error::anyhow!("Error Occured : {:?}", err))?;

        let result = loop {
            let events = self.handler().events();
            let event = events.recv_timeout(std::time::Duration::from_secs(30))?;

            if let Event::Liquidity(LiquidityEvent::InvoiceparamsReady {
                counterparty_node_id,
                intercept_scid,
                cltv_expiry_delta,
            }) = event
            {
                break (intercept_scid, cltv_expiry_delta);
            }
        };

        Ok(result)
    }

    pub fn create_jit_invoice(
        &self,
        amount_msat: u64,
        description: String,
    ) -> error::Result<Bolt11Invoice> {
        let fee_params = self.client_request_opening_params()?;
        let best_fee_param = &fee_params.first().unwrap().clone();

        let result = self.buy_request(best_fee_param.clone(), amount_msat)?;
        let invoice =
            self.generate_invoice_for_jit_channel(amount_msat, description, result.0, result.1)?;

        Ok(invoice)
    }

    fn get_lsp_provider(&self) -> LSPManager {
        self.lsp_manager.clone().unwrap()
    }

    fn generate_invoice_for_jit_channel(
        &self,
        amount_msat: u64,
        description: String,
        intercept_scid: u64,
        cltv_expiry_delta: u32,
    ) -> error::Result<Bolt11Invoice> {
        let node_id = self.get_lsp_provider().node_id;

        // TODO: This needs to be configurable
        let expiry_seconds = 300;

        let min_final_cltv_expiry_delta = MIN_FINAL_CLTV_EXPIRY_DELTA + 2;

        let res = self
            .channel_manager
            .channeld
            .clone()
            .unwrap()
            .create_inbound_payment(None, expiry_seconds, Some(min_final_cltv_expiry_delta));

        let payment_hash = res.unwrap().0;
        let payment_secret = res.unwrap().1;

        let route_hint = RouteHint(vec![RouteHintHop {
            src_node_id: node_id,
            short_channel_id: intercept_scid,
            fees: RoutingFees {
                base_msat: 0,
                proportional_millionths: 0,
            },
            cltv_expiry_delta: cltv_expiry_delta as u16,
            htlc_minimum_msat: None,
            htlc_maximum_msat: None,
        }]);

        let payment_hash = sha256::Hash::from_slice(&payment_hash.0)?;

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

    fn handler(&self) -> Arc<LampoHandler> {
        self.handler.lock().unwrap().clone().unwrap()
    }
}
