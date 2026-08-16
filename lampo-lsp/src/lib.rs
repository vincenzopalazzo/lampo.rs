//! Modular LSP plugin for lampo.
//!
//! Wraps LDK's `lightning-liquidity` and plugs into lampod through
//! [`LampoMsgHandler`] and [`ExternalHandler`]. lampod itself never names
//! this crate; `lampod-cli` and `lampo-testing` compose it at the edge.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Notify};

use lightning_liquidity::events::LiquidityEvent;
use lightning_liquidity::lsps0::event::LSPS0ClientEvent;
use lightning_liquidity::lsps0::ser::{RawLSPSMessage, LSPS_MESSAGE_TYPE_ID};
use lightning_liquidity::utils::time::DefaultTimeProvider;
use lightning_liquidity::{LiquidityClientConfig, LiquidityManager, LiquidityServiceConfig};

use lampo_common::async_trait;
use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::error;
use lampo_common::handler::ExternalHandler;
use lampo_common::json;
use lampo_common::jsonrpc::Request;
use lampo_common::keys::LampoKeysManager;
use lampo_common::ldk::ln::msgs::{Init, LightningError};
use lampo_common::ldk::ln::peer_handler::CustomMessageHandler;
use lampo_common::ldk::ln::wire::Type;
use lampo_common::ldk::types::features::{InitFeatures, NodeFeatures};
use lampo_common::ldk::util::ser::{LengthReadable, Writeable};
use lampo_common::model::response::LspInfo;
use lampo_common::msg::{LampoMsgHandler, LampoWireMessage};
use lampo_common::types::LampoChannel;

use lampod::chain::LampoChainManager;
use lampod::ln::LampoPeerManager;
use lampod::persistence::LampoPersistence;
use lampod::LampoDaemon;

type LampoLiquidityManager = LiquidityManager<
    Arc<LampoKeysManager>,
    Arc<LampoKeysManager>,
    Arc<LampoChannel>,
    Arc<LampoPersistence>,
    DefaultTimeProvider,
    Arc<LampoChainManager>,
>;

enum Inner {
    Disabled,
    Enabled(Arc<LampoLiquidityManager>),
}

/// LSP plugin. Always attached; [`LampoConf::lsp_enable`] selects the inner
/// implementation so `lsp-info` stays reachable when the feature is off.
pub struct LampoLsp {
    inner: Inner,
    peer_manager: Option<Arc<LampoPeerManager>>,
    flush: Arc<Notify>,
    /// Serializes `lsps0-list-protocols` so waiters cannot steal each
    /// other's `LiquidityEvent`s.
    list_protocols: Mutex<()>,
    client: bool,
    service: bool,
    advertise: bool,
}

impl LampoLsp {
    /// Build the plugin from a fully initialized daemon and register it.
    ///
    /// Must run after [`LampoDaemon::init`] and before [`LampoDaemon::listen`]
    /// so LSPS feature bits are visible on the first peer connection.
    /// Register this *before* `HttpdHandler` so `lsp-info` does not recurse
    /// through HTTP.
    pub async fn attach(lampod: &LampoDaemon) -> error::Result<Arc<Self>> {
        let plugin = Arc::new(Self::from_daemon(lampod).await?);
        lampod.add_custom_msg_handler(plugin.clone())?;
        lampod.add_external_handler(plugin.clone()).await?;
        plugin.spawn_persist_task();
        Ok(plugin)
    }

    async fn from_daemon(lampod: &LampoDaemon) -> error::Result<Self> {
        let conf = lampod.conf();
        if !conf.lsp_enable {
            log::info!(target: "lampo-lsp", "LSP plugin disabled (experimental)");
            return Ok(Self {
                inner: Inner::Disabled,
                peer_manager: None,
                flush: Arc::new(Notify::new()),
                list_protocols: Mutex::new(()),
                client: false,
                service: false,
                advertise: false,
            });
        }

        log::info!(
            target: "lampo-lsp",
            "enabling experimental LSP plugin (client={}, service={}, advertise={})",
            conf.lsp_client,
            conf.lsp_service,
            conf.lsp_advertise
        );

        let service_config = if conf.lsp_service {
            Some(LiquidityServiceConfig {
                lsps1_service_config: None,
                lsps2_service_config: None,
                lsps5_service_config: None,
                advertise_service: conf.lsp_advertise,
            })
        } else {
            None
        };
        let client_config = if conf.lsp_client {
            Some(LiquidityClientConfig {
                lsps1_client_config: None,
                lsps2_client_config: None,
                lsps5_client_config: None,
            })
        } else {
            None
        };

        let keys = lampod.wallet_manager().ldk_keys().keys_manager.clone();
        let lm = LiquidityManager::new(
            keys.clone(),
            keys,
            lampod.channel_manager().manager(),
            lampod.persister(),
            lampod.onchain_manager(),
            service_config,
            client_config,
        )
        .await
        .map_err(|err| error::anyhow!("failed to construct LiquidityManager: {err}"))?;

        Ok(Self {
            inner: Inner::Enabled(Arc::new(lm)),
            peer_manager: Some(lampod.peer_manager()),
            flush: Arc::new(Notify::new()),
            list_protocols: Mutex::new(()),
            client: conf.lsp_client,
            service: conf.lsp_service,
            advertise: conf.lsp_advertise,
        })
    }

    fn spawn_persist_task(&self) {
        let Inner::Enabled(lm) = &self.inner else {
            return;
        };
        let lm = lm.clone();
        let flush = self.flush.clone();
        let peer_manager = self.peer_manager.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {
                        if let Err(err) = lm.persist().await {
                            log::error!(target: "lampo-lsp", "persist failed: {err}");
                        }
                    }
                    _ = flush.notified() => {
                        // Let PeerManager finish the call that queued the
                        // message (handle_custom_message runs on the socket
                        // task) before draining pending sends.
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        if let Some(peer_manager) = peer_manager.as_ref() {
                            peer_manager.manager().process_events();
                        }
                    }
                }
            }
        });
    }

    fn request_flush(&self) {
        self.flush.notify_one();
    }

    fn lm(&self) -> Option<&LampoLiquidityManager> {
        match &self.inner {
            Inner::Enabled(lm) => Some(lm),
            Inner::Disabled => None,
        }
    }

    fn info(&self) -> LspInfo {
        LspInfo {
            enabled: self.lm().is_some(),
            client: self.client,
            service: self.service,
            advertise: self.advertise,
            experimental: true,
        }
    }

    async fn list_protocols(&self, node_id: PublicKey) -> error::Result<Vec<u16>> {
        let lm = self
            .lm()
            .ok_or_else(|| error::anyhow!("LSP plugin is disabled"))?;
        let _guard = self.list_protocols.lock().await;
        // Drop anything left over from a prior timeout so we cannot
        // return a stale ListProtocolsResponse for this request.
        let stale = lm.get_and_clear_pending_events();
        if !stale.is_empty() {
            log::debug!(
                target: "lampo-lsp",
                "dropping {} stale liquidity event(s) before list_protocols",
                stale.len()
            );
        }
        lm.lsps0_client_handler().list_protocols(&node_id);
        self.request_flush();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                error::bail!("timed out waiting for lsps0.list_protocols response");
            }
            let event = tokio::time::timeout(remaining, lm.next_event_async())
                .await
                .map_err(|_| {
                    error::anyhow!("timed out waiting for lsps0.list_protocols response")
                })?;
            match event {
                LiquidityEvent::LSPS0Client(LSPS0ClientEvent::ListProtocolsResponse {
                    counterparty_node_id,
                    protocols,
                }) if counterparty_node_id == node_id => return Ok(protocols),
                other => {
                    // LSPS0-only: no other RPC consumes this queue yet.
                    log::debug!(target: "lampo-lsp", "ignoring liquidity event while waiting for list_protocols: {other:?}");
                }
            }
        }
    }
}

impl LampoMsgHandler for LampoLsp {
    fn handles(&self, type_id: u16) -> bool {
        self.lm().is_some() && type_id == LSPS_MESSAGE_TYPE_ID
    }

    fn handle_custom_message(
        &self,
        type_id: u16,
        payload: &[u8],
        sender_node_id: PublicKey,
    ) -> Result<(), LightningError> {
        let Some(lm) = self.lm() else {
            return Ok(());
        };
        if type_id != LSPS_MESSAGE_TYPE_ID {
            return Ok(());
        }
        let mut buf = payload;
        let raw = RawLSPSMessage::read_from_fixed_length_buffer(&mut buf).map_err(|err| {
            LightningError {
                err: format!("invalid LSPS0 payload: {err:?}"),
                action: lampo_common::ldk::ln::msgs::ErrorAction::IgnoreAndLog(
                    lampo_common::ldk::util::logger::Level::Info,
                ),
            }
        })?;
        let result = lm.handle_custom_message(raw, sender_node_id);
        self.request_flush();
        result
    }

    fn get_and_clear_pending_msg(&self) -> Vec<(PublicKey, LampoWireMessage)> {
        let Some(lm) = self.lm() else {
            return Vec::new();
        };
        lm.get_and_clear_pending_msg()
            .into_iter()
            .map(|(node_id, raw)| {
                (
                    node_id,
                    LampoWireMessage {
                        type_id: raw.type_id(),
                        payload: raw.encode(),
                    },
                )
            })
            .collect()
    }

    fn peer_disconnected(&self, their_node_id: PublicKey) {
        if let Some(lm) = self.lm() {
            lm.peer_disconnected(their_node_id);
        }
    }

    fn peer_connected(
        &self,
        their_node_id: PublicKey,
        msg: &Init,
        inbound: bool,
    ) -> Result<(), ()> {
        match self.lm() {
            Some(lm) => lm.peer_connected(their_node_id, msg, inbound),
            None => Ok(()),
        }
    }

    fn provided_node_features(&self) -> NodeFeatures {
        match self.lm() {
            Some(lm) => lm.provided_node_features(),
            None => NodeFeatures::empty(),
        }
    }

    fn provided_init_features(&self, their_node_id: PublicKey) -> InitFeatures {
        match self.lm() {
            Some(lm) => lm.provided_init_features(their_node_id),
            None => InitFeatures::empty(),
        }
    }
}

#[async_trait]
impl ExternalHandler for LampoLsp {
    async fn handle(&self, req: &Request<json::Value>) -> error::Result<Option<json::Value>> {
        match req.method.as_str() {
            "lsp-info" => Ok(Some(json::to_value(self.info())?)),
            "lsps0-list-protocols" => {
                let node_id = req
                    .params
                    .get("node_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| error::anyhow!("missing node_id"))?;
                let node_id = PublicKey::from_str(node_id)
                    .map_err(|err| error::anyhow!("invalid node_id: {err}"))?;
                let protocols = self.list_protocols(node_id).await?;
                Ok(Some(json::json!({ "protocols": protocols })))
            }
            _ => Ok(None),
        }
    }
}
