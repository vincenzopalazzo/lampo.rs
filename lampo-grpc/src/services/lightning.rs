use std::sync::Arc;
use tonic::{Request, Response, Status};
use lampod::LampoDaemon;
use lampod::jsonrpc::inventory::json_getinfo;
use lampod::jsonrpc::onchain::json_new_addr;
use lampod::jsonrpc::channels::json_channels;
use lampod::jsonrpc::peer_control::json_connect;
use lampod::jsonrpc::offchain::{json_invoice, json_decode, json_pay};
use lampo_common::model::GetInfo;
use lampo_common::model::response;
use serde_json;
use crate::auth::MacaroonManager;

pub mod lnrpc {
    tonic::include_proto!("lnrpc");
}

use lnrpc::lightning_server::Lightning;
use lnrpc::*;

pub struct LightningService {
    daemon: Arc<LampoDaemon>,
    macaroon_manager: Arc<MacaroonManager>,
}

impl LightningService {
    pub fn new(daemon: Arc<LampoDaemon>, macaroon_manager: Arc<MacaroonManager>) -> Self {
        Self {
            daemon,
            macaroon_manager,
        }
    }
}

#[tonic::async_trait]
impl Lightning for LightningService {
    async fn get_info(
        &self,
        _request: Request<GetInfoRequest>,
    ) -> Result<Response<GetInfoResponse>, Status> {
        log::info!("GetInfo called");
        
        self.macaroon_manager.validate_request(&_request)?;
        
        // Call existing lampod jsonrpc method
        let result = json_getinfo(&self.daemon, &serde_json::json!({})).await
            .map_err(|e| Status::internal(format!("getinfo failed: {}", e)))?;
        
        let lampo_info: GetInfo = serde_json::from_value(result)
            .map_err(|e| Status::internal(format!("Failed to parse response: {}", e)))?;
        
        // mapping it to lnd format
        let response = GetInfoResponse {
            version: format!("{}-lnd-compat", env!("CARGO_PKG_VERSION")),
            commit_hash: option_env!("VERGEN_GIT_SHA").unwrap_or(env!("CARGO_PKG_NAME")).to_string(),
            identity_pubkey: lampo_info.node_id,
            alias: lampo_info.alias,
            color: lampo_info.color,
            num_peers: lampo_info.peers as u32,
            num_active_channels: lampo_info.channels as u32,
            num_pending_channels: 0,
            num_inactive_channels: 0,
            block_height: lampo_info.blockheight,
            block_hash: lampo_info.block_hash,
            synced_to_chain: lampo_info.wallet_height >= lampo_info.blockheight as u64,
            synced_to_graph: true,
            testnet: lampo_info.chain != "mainnet",
            chains: vec![lnrpc::Chain {
                chain: "bitcoin".to_string(),
                network: lampo_info.chain,
            }],
            uris: vec![],
            features: Default::default(),
            require_htlc_interceptor: false,
            store_final_htlc_resolutions: false,
            best_header_timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        };
        
        log::info!("GetInfo response: identity_pubkey={}, alias={}, channels={}, peers={}", 
                  response.identity_pubkey, response.alias, response.num_active_channels, response.num_peers);
        
        Ok(Response::new(response))
    }

    async fn wallet_balance(
        &self,
        _request: Request<WalletBalanceRequest>,
    ) -> Result<Response<WalletBalanceResponse>, Status> {
        log::info!("WalletBalance called");
        
        // Validate authentication
        self.macaroon_manager.validate_request(&_request)?;
        
        // Get wallet balance from Lampo 
        let balance = self.daemon.wallet_manager().get_onchain_balance().await
            .map_err(|e| Status::internal(format!("Failed to get wallet balance: {}", e)))?;
        
        let confirmed_balance = balance as i64;
        let unconfirmed_balance = 0i64; // BDK get_onchain_balance only returns confirmed
        let total_balance = confirmed_balance;
        
        let response = WalletBalanceResponse {
            total_balance,
            confirmed_balance,
            unconfirmed_balance,
            locked_balance: 0, // BDK does not track locked balance separately
            reserved_balance_anchor_chan: 0, // No anchor reserves in current implementation
            account_balance: Default::default(),
        };
        
        log::info!("WalletBalance response: total={}, confirmed={}", 
                  response.total_balance, response.confirmed_balance);
        
        Ok(Response::new(response))
    }

    async fn channel_balance(
        &self,
        _request: Request<ChannelBalanceRequest>,
    ) -> Result<Response<ChannelBalanceResponse>, Status> {
        log::info!("ChannelBalance called");
        
        // Validate authentication  
        self.macaroon_manager.validate_request(&_request)?;
        
        // Get channel info from Lampo
        let channels = self.daemon.channel_manager().manager().list_channels();
        
        let mut balance: i64 = 0;
        let pending_open_balance: i64 = 0;
        
        for channel in &channels {
            // Add outbound capacity (what we can send)
            balance += channel.outbound_capacity_msat as i64 / 1000; // Convert to sats
        }
        
        let response = ChannelBalanceResponse {
            balance,
            pending_open_balance,
            local_balance: None,
            remote_balance: None,
            unsettled_local_balance: None,
            unsettled_remote_balance: None,
            pending_open_local_balance: None,
            pending_open_remote_balance: None,
        };
        
        log::info!("ChannelBalance response: balance={}, channels={}", 
                  response.balance, channels.len());
        
        Ok(Response::new(response))
    }

    async fn get_transactions(
        &self,
        _request: Request<GetTransactionsRequest>,
    ) -> Result<Response<TransactionDetails>, Status> {
        Err(Status::unimplemented("get_transactions"))
    }

    async fn estimate_fee(
        &self,
        _request: Request<EstimateFeeRequest>,
    ) -> Result<Response<EstimateFeeResponse>, Status> {
        Err(Status::unimplemented("estimate_fee"))
    }

    async fn send_coins(
        &self,
        _request: Request<SendCoinsRequest>,
    ) -> Result<Response<SendCoinsResponse>, Status> {
        Err(Status::unimplemented("send_coins"))
    }

    async fn list_unspent(
        &self,
        _request: Request<ListUnspentRequest>,
    ) -> Result<Response<ListUnspentResponse>, Status> {
        Err(Status::unimplemented("list_unspent"))
    }

    async fn send_many(
        &self,
        _request: Request<SendManyRequest>,
    ) -> Result<Response<SendManyResponse>, Status> {
        Err(Status::unimplemented("send_many"))
    }

    async fn new_address(
        &self,
        _request: Request<NewAddressRequest>,
    ) -> Result<Response<NewAddressResponse>, Status> {
        log::info!("NewAddress called");
        
        // Validate authentication
        self.macaroon_manager.validate_request(&_request)?;
        
        // Call existing lampod jsonrpc method
        let result = json_new_addr(&self.daemon, &serde_json::json!({})).await
            .map_err(|e| Status::internal(format!("new_addr failed: {}", e)))?;
        
        // Extract address from the response
        let address = result.get("address")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Status::internal("Invalid new_addr response format"))?;
        
        let response = NewAddressResponse {
            address: address.to_string(),
        };
        
        log::info!("NewAddress response: address={}", response.address);
        
        Ok(Response::new(response))
    }

    async fn sign_message(
        &self,
        _request: Request<SignMessageRequest>,
    ) -> Result<Response<SignMessageResponse>, Status> {
        Err(Status::unimplemented("sign_message"))
    }

    async fn verify_message(
        &self,
        _request: Request<VerifyMessageRequest>,
    ) -> Result<Response<VerifyMessageResponse>, Status> {
        Err(Status::unimplemented("verify_message"))
    }

    async fn connect_peer(
        &self,
        _request: Request<ConnectPeerRequest>,
    ) -> Result<Response<ConnectPeerResponse>, Status> {
        log::info!("ConnectPeer called");
        
        // Validate authentication  
        self.macaroon_manager.validate_request(&_request)?;
        
        let req = _request.into_inner();
        let addr = req.addr.ok_or_else(|| Status::invalid_argument("Missing addr in request"))?;
        
        // Format connection string for lampod jsonrpc (node_id@host:port)
        let connection_string = format!("{}@{}", addr.pubkey, addr.host);
        
        // Call existing lampod jsonrpc method
        let connect_request = serde_json::json!({
            "node_id": connection_string
        });
        
        json_connect(&self.daemon, &connect_request).await
            .map_err(|e| Status::internal(format!("connect failed: {}", e)))?;
        
        log::info!("ConnectPeer successful: {}", connection_string);
        
        Ok(Response::new(ConnectPeerResponse {}))
    }

    async fn disconnect_peer(
        &self,
        _request: Request<DisconnectPeerRequest>,
    ) -> Result<Response<DisconnectPeerResponse>, Status> {
        Err(Status::unimplemented("disconnect_peer"))
    }

    async fn list_peers(
        &self,
        _request: Request<ListPeersRequest>,
    ) -> Result<Response<ListPeersResponse>, Status> {
        log::info!("ListPeers called");
        
        // Validate authentication
        self.macaroon_manager.validate_request(&_request)?;
        
        // Get peers from Lampo
        let lampo_peers = self.daemon.peer_manager().manager().list_peers();
        
        // Convert Lampo peers to LND format
        let mut lnd_peers = Vec::new();

        //TODO: Get actual peer info from Lampo
        for lampo_peer in lampo_peers {
            let lnd_peer = lnrpc::Peer {
                pub_key: lampo_peer.counterparty_node_id.to_string(), // Convert NodeId to string
                address: String::new(), 
                bytes_sent: 0, 
                bytes_recv: 0,  
                sat_sent: 0, 
                sat_recv: 0, 
                inbound: false, 
                ping_time: 0, 
                sync_type: 0, 
                features: Default::default(), 
                errors: vec![], 
                flap_count: 0, 
                last_flap_ns: 0, 
                last_ping_payload: vec![], 
            };
            lnd_peers.push(lnd_peer);
        }
        
        let response = ListPeersResponse {
            peers: lnd_peers,
        };
        
        log::info!("ListPeers response: {} peers", response.peers.len());
        
        Ok(Response::new(response))
    }

    async fn get_recovery_info(
        &self,
        _request: Request<GetRecoveryInfoRequest>,
    ) -> Result<Response<GetRecoveryInfoResponse>, Status> {
        Err(Status::unimplemented("get_recovery_info"))
    }

    async fn pending_channels(
        &self,
        _request: Request<PendingChannelsRequest>,
    ) -> Result<Response<PendingChannelsResponse>, Status> {
        Err(Status::unimplemented("pending_channels"))
    }

    async fn list_channels(
        &self,
        _request: Request<ListChannelsRequest>,
    ) -> Result<Response<ListChannelsResponse>, Status> {
        log::info!("ListChannels called");
        
        // Validate authentication
        self.macaroon_manager.validate_request(&_request)?;
        
        // Call existing lampod jsonrpc method
        let result = json_channels(&self.daemon, &serde_json::json!({})).await
            .map_err(|e| Status::internal(format!("list_channels failed: {}", e)))?;
        
        // Parse the lampod channels response
        let lampo_channels: response::Channels = serde_json::from_value(result)
            .map_err(|e| Status::internal(format!("Failed to parse channels response: {}", e)))?;
        
        // Convert lampod channels to LND format
        let mut lnd_channels = Vec::new();
        for lampo_channel in lampo_channels.channels {
            let chan_id = lampo_channel.short_channel_id.unwrap_or(0);
            
            // Calculate balances (convert from msat to sat)
            let local_balance = (lampo_channel.available_balance_for_send_msat / 1000) as i64;
            let remote_balance = (lampo_channel.available_balance_for_recv_msat / 1000) as i64;
            let capacity = lampo_channel.amount as i64;
            
            let lnd_channel = lnrpc::Channel {
                active: lampo_channel.ready,
                remote_pubkey: lampo_channel.peer_id,
                channel_point: format!("{}:0", lampo_channel.channel_id),
                chan_id,
                capacity,
                local_balance,
                remote_balance,
                commit_fee: 0,
                commit_weight: 0,
                fee_per_kw: 0,
                unsettled_balance: 0,
                total_satoshis_sent: 0,
                total_satoshis_received: 0,
                num_updates: 0,
                pending_htlcs: vec![],
                local_chan_reserve_sat: 0,
                remote_chan_reserve_sat: 0,
                static_remote_key: false,
                commitment_type: 0,
                lifetime: 0,
                uptime: 0,
                close_address: String::new(),
                push_amount_sat: 0,
                thaw_height: 0,
                local_constraints: None,
                remote_constraints: None,
                alias_scids: vec![],
                zero_conf: false,
                zero_conf_confirmed_scid: 0,
                peer_alias: lampo_channel.peer_alias.unwrap_or_default(),
                peer_scid_alias: 0,
                memo: String::new(),
                private: !lampo_channel.public,
                initiator: false,
                chan_status_flags: String::new(),
                csv_delay: 144, // Standard CSV delay for Lightning channels
            };
            lnd_channels.push(lnd_channel);
        }
        
        let response = ListChannelsResponse {
            channels: lnd_channels,
        };
        
        log::info!("ListChannels response: {} channels", response.channels.len());
        
        Ok(Response::new(response))
    }

    async fn closed_channels(
        &self,
        _request: Request<ClosedChannelsRequest>,
    ) -> Result<Response<ClosedChannelsResponse>, Status> {
        Err(Status::unimplemented("closed_channels"))
    }

    async fn open_channel_sync(
        &self,
        _request: Request<OpenChannelRequest>,
    ) -> Result<Response<ChannelPoint>, Status> {
        Err(Status::unimplemented("open_channel_sync"))
    }

    async fn batch_open_channel(
        &self,
        _request: Request<BatchOpenChannelRequest>,
    ) -> Result<Response<BatchOpenChannelResponse>, Status> {
        Err(Status::unimplemented("batch_open_channel"))
    }

    async fn funding_state_step(
        &self,
        _request: Request<FundingTransitionMsg>,
    ) -> Result<Response<FundingStateStepResp>, Status> {
        Err(Status::unimplemented("funding_state_step"))
    }

    async fn abandon_channel(
        &self,
        _request: Request<AbandonChannelRequest>,
    ) -> Result<Response<AbandonChannelResponse>, Status> {
        Err(Status::unimplemented("abandon_channel"))
    }

    async fn send_payment_sync(
        &self,
        _request: Request<SendRequest>,
    ) -> Result<Response<SendResponse>, Status> {
        log::info!("SendPaymentSync called");
        
        // Validate authentication (admin required for sending payments)
        let permission = self.macaroon_manager.validate_request(&_request)?;
        if permission != crate::auth::MacaroonPermission::Admin {
            return Err(Status::permission_denied("Admin access required for sending payments"));
        }
        
        let req = _request.into_inner();
        
        // Convert LND SendRequest to Lampo pay format
        let lampo_request = serde_json::json!({
            "invoice_str": req.payment_request,
            "amount": if req.amt > 0 { Some(req.amt as u64 * 1000) } else { None } // Convert sats to msats if specified
        });
        
        // Call existing lampod jsonrpc method
        let pay_result = json_pay(&self.daemon, &lampo_request).await
            .map_err(|e| Status::internal(format!("payment failed: {}", e)))?;
        
        // Parse the payment result and extract response data
        let payment_preimage = if let Some(preimage_str) = pay_result.get("payment_preimage").and_then(|v| v.as_str()) {
            hex::decode(preimage_str).unwrap_or_default()
        } else {
            vec![]
        };
        
        let payment_hash = if let Some(hash_str) = pay_result.get("payment_hash").and_then(|v| v.as_str()) {
            hex::decode(hash_str).unwrap_or_default()
        } else {
            let decode_request = serde_json::json!({
                "invoice_str": req.payment_request
            });
            if let Ok(decode_result) = json_decode(&self.daemon, &decode_request).await {
                if let Some(hash_str) = decode_result.get("payment_hash").and_then(|v| v.as_str()) {
                    hex::decode(hash_str).unwrap_or_default()
                } else {
                    vec![]
                }
            } else {
                vec![]
            }
        };
        
        let response = SendResponse {
            payment_error: String::new(), // Empty means success
            payment_preimage,
            payment_route: None, // Route details not available 
            payment_hash,
        };
        
        log::info!("SendPaymentSync completed for invoice: {}", req.payment_request);
        
        Ok(Response::new(response))
    }

    async fn send_to_route_sync(
        &self,
        _request: Request<SendToRouteRequest>,
    ) -> Result<Response<SendResponse>, Status> {
        Err(Status::unimplemented("send_to_route_sync"))
    }

    async fn add_invoice(
        &self,
        _request: Request<Invoice>,
    ) -> Result<Response<AddInvoiceResponse>, Status> {
        log::info!("AddInvoice called");
        
        // Validate authentication (admin required for creating invoices)
        let permission = self.macaroon_manager.validate_request(&_request)?;
        if permission != crate::auth::MacaroonPermission::Admin {
            return Err(Status::permission_denied("Admin access required for creating invoices"));
        }
        
        let req = _request.into_inner();
        
        // Convert LND Invoice request to Lampo GenerateInvoice format
        let lampo_request = serde_json::json!({
            "amount_msat": if req.value > 0 { Some(req.value as u64 * 1000) } else { None }, // Convert sats to msats
            "description": req.memo,
            "expiring_in": if req.expiry > 0 { Some(req.expiry as u32) } else { Some(3600) } // Default 1 hour
        });
        
        // Call existing lampod jsonrpc method
        let result = json_invoice(&self.daemon, &lampo_request).await
            .map_err(|e| Status::internal(format!("invoice generation failed: {}", e)))?;
        
        // Parse the response to extract the bolt11 string
        let bolt11 = result.get("bolt11")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Status::internal("Invalid invoice response format"))?;
        
        // Extract the payment hash from the bolt11 invoice using lampod's decode function
        let decode_request = serde_json::json!({
            "invoice_str": bolt11
        });
        
        let decode_result = json_decode(&self.daemon, &decode_request).await
            .map_err(|e| Status::internal(format!("Failed to decode invoice for hash extraction: {}", e)))?;
        
        // Extract payment hash from decoded invoice - it should be in the result
        let r_hash = if let Some(hash_str) = decode_result.get("payment_hash").and_then(|v| v.as_str()) {
            hex::decode(hash_str).unwrap_or_else(|_| vec![0u8; 32])
        } else {
            // Fallback: generate hash from bolt11 string as bytes
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            bolt11.hash(&mut hasher);
            let hash_u64 = hasher.finish();
            let mut hash_bytes = vec![0u8; 32];
            hash_bytes[..8].copy_from_slice(&hash_u64.to_be_bytes());
            hash_bytes
        };
        
        let response = AddInvoiceResponse {
            r_hash,
            payment_request: bolt11.to_string(),
            add_index: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64, // Use timestamp as unique add index
            payment_addr: vec![], // Payment address not used in current implementation
        };
        
        log::info!("AddInvoice response: payment_request={}", response.payment_request);
        
        Ok(Response::new(response))
    }

    async fn list_invoices(
        &self,
        _request: Request<ListInvoiceRequest>,
    ) -> Result<Response<ListInvoiceResponse>, Status> {
        Err(Status::unimplemented("list_invoices"))
    }

    async fn lookup_invoice(
        &self,
        _request: Request<PaymentHash>,
    ) -> Result<Response<Invoice>, Status> {
        log::info!("LookupInvoice called");
        
        // Validate authentication
        self.macaroon_manager.validate_request(&_request)?;
        
        let _req = _request.into_inner();
        
        // TODO: Implement invoice lookup in lampod
        
        Err(Status::unimplemented("Invoice lookup not yet implemented in lampod - requires invoice storage"))
    }

    async fn decode_pay_req(
        &self,
        _request: Request<PayReqString>,
    ) -> Result<Response<PayReq>, Status> {
        log::info!("DecodePayReq called");
        
        // Validate authentication (readonly access allowed)
        self.macaroon_manager.validate_request(&_request)?;
        
        let req = _request.into_inner();
        
        // Convert to lampod decode request format
        let lampo_request = serde_json::json!({
            "invoice_str": req.pay_req
        });
        
        // Call existing lampod jsonrpc method
        let result = json_decode(&self.daemon, &lampo_request).await
            .map_err(|e| Status::internal(format!("decode failed: {}", e)))?;
        
        // Parse the decode response and extract all fields from Bolt11InvoiceInfo
        let destination = result.get("issuer_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        
        let payment_hash = result.get("payment_hash")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        
        let amount_msat = result.get("amount_msat")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        
        let num_satoshis = (amount_msat / 1000) as i64;
        let num_msat = amount_msat as i64;
        
        let description = result.get("description")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        
        let expiry_time = result.get("expiry_time")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        
        // Calculate expiry from current time and expiry_time
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        let response = PayReq {
            destination,
            payment_hash,
            num_satoshis,
            timestamp: current_time as i64, // Current timestamp as decode time
            expiry: (expiry_time / 1000) as i64, // Convert ms to seconds
            description,
            description_hash: String::new(), // Not commonly used
            fallback_addr: String::new(), // Not extracted from current decode
            cltv_expiry: 0, // Not in current decode response
            route_hints: vec![], // Not in current decode response
            payment_addr: vec![], // Not in current decode response
            num_msat,
            features: Default::default(), // Features not in current decode response
        };
        
        log::info!("DecodePayReq response for: {}", req.pay_req);
        
        Ok(Response::new(response))
    }

    async fn list_payments(
        &self,
        _request: Request<ListPaymentsRequest>,
    ) -> Result<Response<ListPaymentsResponse>, Status> {
        Err(Status::unimplemented("list_payments"))
    }

    async fn delete_payment(
        &self,
        _request: Request<DeletePaymentRequest>,
    ) -> Result<Response<DeletePaymentResponse>, Status> {
        Err(Status::unimplemented("delete_payment"))
    }

    async fn delete_all_payments(
        &self,
        _request: Request<DeleteAllPaymentsRequest>,
    ) -> Result<Response<DeleteAllPaymentsResponse>, Status> {
        Err(Status::unimplemented("delete_all_payments"))
    }

    async fn describe_graph(
        &self,
        _request: Request<ChannelGraphRequest>,
    ) -> Result<Response<ChannelGraph>, Status> {
        Err(Status::unimplemented("describe_graph"))
    }

    async fn get_node_metrics(
        &self,
        _request: Request<NodeMetricsRequest>,
    ) -> Result<Response<NodeMetricsResponse>, Status> {
        Err(Status::unimplemented("get_node_metrics"))
    }

    async fn get_chan_info(
        &self,
        _request: Request<ChanInfoRequest>,
    ) -> Result<Response<ChannelEdge>, Status> {
        Err(Status::unimplemented("get_chan_info"))
    }

    async fn get_node_info(
        &self,
        _request: Request<NodeInfoRequest>,
    ) -> Result<Response<NodeInfo>, Status> {
        Err(Status::unimplemented("get_node_info"))
    }

    async fn query_routes(
        &self,
        _request: Request<QueryRoutesRequest>,
    ) -> Result<Response<QueryRoutesResponse>, Status> {
        Err(Status::unimplemented("query_routes"))
    }

    async fn get_network_info(
        &self,
        _request: Request<NetworkInfoRequest>,
    ) -> Result<Response<NetworkInfo>, Status> {
        Err(Status::unimplemented("get_network_info"))
    }

    async fn stop_daemon(
        &self,
        _request: Request<StopRequest>,
    ) -> Result<Response<StopResponse>, Status> {
        Err(Status::unimplemented("stop_daemon"))
    }

    async fn debug_level(
        &self,
        _request: Request<DebugLevelRequest>,
    ) -> Result<Response<DebugLevelResponse>, Status> {
        Err(Status::unimplemented("debug_level"))
    }

    async fn fee_report(
        &self,
        _request: Request<FeeReportRequest>,
    ) -> Result<Response<FeeReportResponse>, Status> {
        Err(Status::unimplemented("fee_report"))
    }

    async fn update_channel_policy(
        &self,
        _request: Request<PolicyUpdateRequest>,
    ) -> Result<Response<PolicyUpdateResponse>, Status> {
        Err(Status::unimplemented("update_channel_policy"))
    }

    async fn forwarding_history(
        &self,
        _request: Request<ForwardingHistoryRequest>,
    ) -> Result<Response<ForwardingHistoryResponse>, Status> {
        Err(Status::unimplemented("forwarding_history"))
    }

    async fn export_channel_backup(
        &self,
        _request: Request<ExportChannelBackupRequest>,
    ) -> Result<Response<ChannelBackup>, Status> {
        Err(Status::unimplemented("export_channel_backup"))
    }

    async fn export_all_channel_backups(
        &self,
        _request: Request<ChanBackupExportRequest>,
    ) -> Result<Response<ChanBackupSnapshot>, Status> {
        Err(Status::unimplemented("export_all_channel_backups"))
    }

    async fn verify_chan_backup(
        &self,
        _request: Request<ChanBackupSnapshot>,
    ) -> Result<Response<VerifyChanBackupResponse>, Status> {
        Err(Status::unimplemented("verify_chan_backup"))
    }

    async fn restore_channel_backups(
        &self,
        _request: Request<RestoreChanBackupRequest>,
    ) -> Result<Response<RestoreBackupResponse>, Status> {
        Err(Status::unimplemented("restore_channel_backups"))
    }

    async fn bake_macaroon(
        &self,
        _request: Request<BakeMacaroonRequest>,
    ) -> Result<Response<BakeMacaroonResponse>, Status> {
        Err(Status::unimplemented("bake_macaroon"))
    }

    async fn list_macaroon_i_ds(
        &self,
        _request: Request<ListMacaroonIDsRequest>,
    ) -> Result<Response<ListMacaroonIDsResponse>, Status> {
        Err(Status::unimplemented("list_macaroon_i_ds"))
    }

    async fn delete_macaroon_id(
        &self,
        _request: Request<DeleteMacaroonIdRequest>,
    ) -> Result<Response<DeleteMacaroonIdResponse>, Status> {
        Err(Status::unimplemented("delete_macaroon_id"))
    }

    async fn list_permissions(
        &self,
        _request: Request<ListPermissionsRequest>,
    ) -> Result<Response<ListPermissionsResponse>, Status> {
        Err(Status::unimplemented("list_permissions"))
    }

    async fn check_macaroon_permissions(
        &self,
        _request: Request<CheckMacPermRequest>,
    ) -> Result<Response<CheckMacPermResponse>, Status> {
        Err(Status::unimplemented("check_macaroon_permissions"))
    }

    async fn send_custom_message(
        &self,
        _request: Request<SendCustomMessageRequest>,
    ) -> Result<Response<SendCustomMessageResponse>, Status> {
        Err(Status::unimplemented("send_custom_message"))
    }

    async fn list_aliases(
        &self,
        _request: Request<ListAliasesRequest>,
    ) -> Result<Response<ListAliasesResponse>, Status> {
        Err(Status::unimplemented("list_aliases"))
    }

    async fn lookup_htlc_resolution(
        &self,
        _request: Request<LookupHtlcResolutionRequest>,
    ) -> Result<Response<LookupHtlcResolutionResponse>, Status> {
        Err(Status::unimplemented("lookup_htlc_resolution"))
    }

    // Streaming methods - will implement later
    type SubscribeTransactionsStream = tokio_stream::wrappers::ReceiverStream<Result<Transaction, Status>>;
    type SubscribePeerEventsStream = tokio_stream::wrappers::ReceiverStream<Result<PeerEvent, Status>>;
    type SubscribeChannelEventsStream = tokio_stream::wrappers::ReceiverStream<Result<ChannelEventUpdate, Status>>;
    type OpenChannelStream = tokio_stream::wrappers::ReceiverStream<Result<OpenStatusUpdate, Status>>;
    type CloseChannelStream = tokio_stream::wrappers::ReceiverStream<Result<CloseStatusUpdate, Status>>;
    type SendPaymentStream = tokio_stream::wrappers::ReceiverStream<Result<SendResponse, Status>>;
    type SendToRouteStream = tokio_stream::wrappers::ReceiverStream<Result<SendResponse, Status>>;
    type SubscribeInvoicesStream = tokio_stream::wrappers::ReceiverStream<Result<Invoice, Status>>;
    type SubscribeChannelGraphStream = tokio_stream::wrappers::ReceiverStream<Result<GraphTopologyUpdate, Status>>;
    type SubscribeChannelBackupsStream = tokio_stream::wrappers::ReceiverStream<Result<ChanBackupSnapshot, Status>>;
    type SubscribeCustomMessagesStream = tokio_stream::wrappers::ReceiverStream<Result<CustomMessage, Status>>;
    type ChannelAcceptorStream = tokio_stream::wrappers::ReceiverStream<Result<ChannelAcceptRequest, Status>>;
    type RegisterRPCMiddlewareStream = tokio_stream::wrappers::ReceiverStream<Result<RpcMiddlewareRequest, Status>>;

    async fn subscribe_transactions(
        &self,
        _request: Request<GetTransactionsRequest>,
    ) -> Result<Response<Self::SubscribeTransactionsStream>, Status> {
        Err(Status::unimplemented("subscribe_transactions"))
    }

    async fn subscribe_peer_events(
        &self,
        _request: Request<PeerEventSubscription>,
    ) -> Result<Response<Self::SubscribePeerEventsStream>, Status> {
        Err(Status::unimplemented("subscribe_peer_events"))
    }

    async fn subscribe_channel_events(
        &self,
        _request: Request<ChannelEventSubscription>,
    ) -> Result<Response<Self::SubscribeChannelEventsStream>, Status> {
        Err(Status::unimplemented("subscribe_channel_events"))
    }

    async fn open_channel(
        &self,
        _request: Request<OpenChannelRequest>,
    ) -> Result<Response<Self::OpenChannelStream>, Status> {
        Err(Status::unimplemented("open_channel"))
    }

    async fn close_channel(
        &self,
        _request: Request<CloseChannelRequest>,
    ) -> Result<Response<Self::CloseChannelStream>, Status> {
        Err(Status::unimplemented("close_channel"))
    }

    async fn send_payment(
        &self,
        _request: Request<tonic::Streaming<SendRequest>>,
    ) -> Result<Response<Self::SendPaymentStream>, Status> {
        Err(Status::unimplemented("send_payment"))
    }

    async fn send_to_route(
        &self,
        _request: Request<tonic::Streaming<SendToRouteRequest>>,
    ) -> Result<Response<Self::SendToRouteStream>, Status> {
        Err(Status::unimplemented("send_to_route"))
    }

    async fn subscribe_invoices(
        &self,
        _request: Request<InvoiceSubscription>,
    ) -> Result<Response<Self::SubscribeInvoicesStream>, Status> {
        Err(Status::unimplemented("subscribe_invoices"))
    }

    async fn subscribe_channel_graph(
        &self,
        _request: Request<GraphTopologySubscription>,
    ) -> Result<Response<Self::SubscribeChannelGraphStream>, Status> {
        Err(Status::unimplemented("subscribe_channel_graph"))
    }

    async fn subscribe_channel_backups(
        &self,
        _request: Request<ChannelBackupSubscription>,
    ) -> Result<Response<Self::SubscribeChannelBackupsStream>, Status> {
        Err(Status::unimplemented("subscribe_channel_backups"))
    }

    async fn subscribe_custom_messages(
        &self,
        _request: Request<SubscribeCustomMessagesRequest>,
    ) -> Result<Response<Self::SubscribeCustomMessagesStream>, Status> {
        Err(Status::unimplemented("subscribe_custom_messages"))
    }

    async fn channel_acceptor(
        &self,
        _request: Request<tonic::Streaming<ChannelAcceptResponse>>,
    ) -> Result<Response<Self::ChannelAcceptorStream>, Status> {
        Err(Status::unimplemented("channel_acceptor"))
    }

    async fn register_rpc_middleware(
        &self,
        _request: Request<tonic::Streaming<RpcMiddlewareResponse>>,
    ) -> Result<Response<Self::RegisterRPCMiddlewareStream>, Status> {
        Err(Status::unimplemented("register_rpc_middleware"))
    }
}