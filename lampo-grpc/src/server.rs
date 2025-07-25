use std::sync::Arc;
use tonic::{Request, Response, Status, Streaming};

use lampod::LampoDaemon;
use lampo_common::model::{request as lampo_req, response as lampo_resp};

pub mod lnrpc {
    tonic::include_proto!("lnrpc");
}
use lnrpc::lightning_server::Lightning;
use lnrpc::*; 

pub struct GrpcServer {
    pub(crate) daemon: Arc<LampoDaemon>,
}

#[tonic::async_trait]
impl Lightning for GrpcServer {
    async fn get_info(
        &self,
        _request: Request<GetInfoRequest>,
    ) -> Result<Response<GetInfoResponse>, Status> {
        let result_json = self.daemon
            .call("getinfo", serde_json::json!({}))
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        let lampo_info: lampo_resp::GetInfo = serde_json::from_value(result_json)
            .map_err(|e| Status::internal(format!("Failed to deserialize daemon response: {}", e)))?;

        let grpc_response = GetInfoResponse {
            identity_pubkey: lampo_info.node_id,
            alias: lampo_info.alias,
            num_active_channels: lampo_info.channels as u32,
            num_peers: lampo_info.peers as u32,
            block_height: lampo_info.blockheight,
            block_hash: lampo_info.block_hash,
            synced_to_chain: lampo_info.wallet_height >= lampo_info.blockheight as u64,
            chains: vec![Chain {
                chain: "bitcoin".to_string(),
                network: lampo_info.chain,
            }],
            version: env!("CARGO_PKG_VERSION").to_string(),
            ..Default::default()
        };

        Ok(Response::new(grpc_response))
    }

    async fn connect_peer(
        &self,
        request: Request<ConnectPeerRequest>,
    ) -> Result<Response<ConnectPeerResponse>, Status> {
        let req = request.into_inner();
        let lnd_addr = req.addr.ok_or_else(|| Status::invalid_argument("LightningAddress is required"))?;
        
        let (host, port_str) = lnd_addr.host.rsplit_once(':').ok_or_else(|| Status::invalid_argument("Address must contain host:port"))?;
        let port = port_str.parse::<u64>().map_err(|_| Status::invalid_argument("Invalid port"))?;

        let lampo_connect_req = lampo_req::Connect {
            node_id: lnd_addr.pubkey,
            addr: host.to_string(),
            port,
        };

        let req_json = serde_json::to_value(lampo_connect_req)
            .map_err(|e| Status::internal(format!("Serialization error: {}", e)))?;

        self.daemon
            .call("connect", req_json)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(ConnectPeerResponse {}))
    }

    // stubbed methods (we will implemenet a macro later)
    async fn wallet_balance(&self, _: Request<WalletBalanceRequest>) -> Result<Response<WalletBalanceResponse>, Status> { Err(Status::unimplemented("wallet_balance")) }
    async fn channel_balance(&self, _: Request<ChannelBalanceRequest>) -> Result<Response<ChannelBalanceResponse>, Status> { Err(Status::unimplemented("channel_balance")) }
    async fn get_transactions(&self, _: Request<GetTransactionsRequest>) -> Result<Response<TransactionDetails>, Status> { Err(Status::unimplemented("get_transactions")) }
    async fn estimate_fee(&self, _: Request<EstimateFeeRequest>) -> Result<Response<EstimateFeeResponse>, Status> { Err(Status::unimplemented("estimate_fee")) }
    async fn send_coins(&self, _: Request<SendCoinsRequest>) -> Result<Response<SendCoinsResponse>, Status> { Err(Status::unimplemented("send_coins")) }
    async fn list_unspent(&self, _: Request<ListUnspentRequest>) -> Result<Response<ListUnspentResponse>, Status> { Err(Status::unimplemented("list_unspent")) }
    async fn send_many(&self, _: Request<SendManyRequest>) -> Result<Response<SendManyResponse>, Status> { Err(Status::unimplemented("send_many")) }
    async fn new_address(&self, _: Request<NewAddressRequest>) -> Result<Response<NewAddressResponse>, Status> { Err(Status::unimplemented("new_address")) }
    async fn sign_message(&self, _: Request<SignMessageRequest>) -> Result<Response<SignMessageResponse>, Status> { Err(Status::unimplemented("sign_message")) }
    async fn verify_message(&self, _: Request<VerifyMessageRequest>) -> Result<Response<VerifyMessageResponse>, Status> { Err(Status::unimplemented("verify_message")) }
    async fn disconnect_peer(&self, _: Request<DisconnectPeerRequest>) -> Result<Response<DisconnectPeerResponse>, Status> { Err(Status::unimplemented("disconnect_peer")) }
    async fn list_peers(&self, _: Request<ListPeersRequest>) -> Result<Response<ListPeersResponse>, Status> { Err(Status::unimplemented("list_peers")) }
    async fn get_recovery_info(&self, _: Request<GetRecoveryInfoRequest>) -> Result<Response<GetRecoveryInfoResponse>, Status> { Err(Status::unimplemented("get_recovery_info")) }
    async fn pending_channels(&self, _: Request<PendingChannelsRequest>) -> Result<Response<PendingChannelsResponse>, Status> { Err(Status::unimplemented("pending_channels")) }
    async fn list_channels(&self, _: Request<ListChannelsRequest>) -> Result<Response<ListChannelsResponse>, Status> { Err(Status::unimplemented("list_channels")) }
    async fn closed_channels(&self, _: Request<ClosedChannelsRequest>) -> Result<Response<ClosedChannelsResponse>, Status> { Err(Status::unimplemented("closed_channels")) }
    async fn open_channel_sync(&self, _: Request<OpenChannelRequest>) -> Result<Response<ChannelPoint>, Status> { Err(Status::unimplemented("open_channel_sync")) }
    async fn batch_open_channel(&self, _: Request<BatchOpenChannelRequest>) -> Result<Response<BatchOpenChannelResponse>, Status> { Err(Status::unimplemented("batch_open_channel")) }
    async fn funding_state_step(&self, _: Request<FundingTransitionMsg>) -> Result<Response<FundingStateStepResp>, Status> { Err(Status::unimplemented("funding_state_step")) }
    async fn abandon_channel(&self, _: Request<AbandonChannelRequest>) -> Result<Response<AbandonChannelResponse>, Status> { Err(Status::unimplemented("abandon_channel")) }
    async fn send_payment_sync(&self, _: Request<SendRequest>) -> Result<Response<SendResponse>, Status> { Err(Status::unimplemented("send_payment_sync")) }
    async fn send_to_route_sync(&self, _: Request<SendToRouteRequest>) -> Result<Response<SendResponse>, Status> { Err(Status::unimplemented("send_to_route_sync")) }
    async fn add_invoice(&self, _: Request<Invoice>) -> Result<Response<AddInvoiceResponse>, Status> { Err(Status::unimplemented("add_invoice")) }
    async fn list_invoices(&self, _: Request<ListInvoiceRequest>) -> Result<Response<ListInvoiceResponse>, Status> { Err(Status::unimplemented("list_invoices")) }
    async fn lookup_invoice(&self, _: Request<PaymentHash>) -> Result<Response<Invoice>, Status> { Err(Status::unimplemented("lookup_invoice")) }
    async fn decode_pay_req(&self, _: Request<PayReqString>) -> Result<Response<PayReq>, Status> { Err(Status::unimplemented("decode_pay_req")) }
    async fn list_payments(&self, _: Request<ListPaymentsRequest>) -> Result<Response<ListPaymentsResponse>, Status> { Err(Status::unimplemented("list_payments")) }
    async fn delete_payment(&self, _: Request<DeletePaymentRequest>) -> Result<Response<DeletePaymentResponse>, Status> { Err(Status::unimplemented("delete_payment")) }
    async fn delete_all_payments(&self, _: Request<DeleteAllPaymentsRequest>) -> Result<Response<DeleteAllPaymentsResponse>, Status> { Err(Status::unimplemented("delete_all_payments")) }
    async fn describe_graph(&self, _: Request<ChannelGraphRequest>) -> Result<Response<ChannelGraph>, Status> { Err(Status::unimplemented("describe_graph")) }
    async fn get_node_metrics(&self, _: Request<NodeMetricsRequest>) -> Result<Response<NodeMetricsResponse>, Status> { Err(Status::unimplemented("get_node_metrics")) }
    async fn get_chan_info(&self, _: Request<ChanInfoRequest>) -> Result<Response<ChannelEdge>, Status> { Err(Status::unimplemented("get_chan_info")) }
    async fn get_node_info(&self, _: Request<NodeInfoRequest>) -> Result<Response<NodeInfo>, Status> { Err(Status::unimplemented("get_node_info")) }
    async fn query_routes(&self, _: Request<QueryRoutesRequest>) -> Result<Response<QueryRoutesResponse>, Status> { Err(Status::unimplemented("query_routes")) }
    async fn get_network_info(&self, _: Request<NetworkInfoRequest>) -> Result<Response<NetworkInfo>, Status> { Err(Status::unimplemented("get_network_info")) }
    async fn stop_daemon(&self, _: Request<StopRequest>) -> Result<Response<StopResponse>, Status> { Err(Status::unimplemented("stop_daemon")) }
    async fn debug_level(&self, _: Request<DebugLevelRequest>) -> Result<Response<DebugLevelResponse>, Status> { Err(Status::unimplemented("debug_level")) }
    async fn fee_report(&self, _: Request<FeeReportRequest>) -> Result<Response<FeeReportResponse>, Status> { Err(Status::unimplemented("fee_report")) }
    async fn update_channel_policy(&self, _: Request<PolicyUpdateRequest>) -> Result<Response<PolicyUpdateResponse>, Status> { Err(Status::unimplemented("update_channel_policy")) }
    async fn forwarding_history(&self, _: Request<ForwardingHistoryRequest>) -> Result<Response<ForwardingHistoryResponse>, Status> { Err(Status::unimplemented("forwarding_history")) }
    async fn export_channel_backup(&self, _: Request<ExportChannelBackupRequest>) -> Result<Response<ChannelBackup>, Status> { Err(Status::unimplemented("export_channel_backup")) }
    async fn export_all_channel_backups(&self, _: Request<ChanBackupExportRequest>) -> Result<Response<ChanBackupSnapshot>, Status> { Err(Status::unimplemented("export_all_channel_backups")) }
    async fn verify_chan_backup(&self, _: Request<ChanBackupSnapshot>) -> Result<Response<VerifyChanBackupResponse>, Status> { Err(Status::unimplemented("verify_chan_backup")) }
    async fn restore_channel_backups(&self, _: Request<RestoreChanBackupRequest>) -> Result<Response<RestoreBackupResponse>, Status> { Err(Status::unimplemented("restore_channel_backups")) }
    async fn bake_macaroon(&self, _: Request<BakeMacaroonRequest>) -> Result<Response<BakeMacaroonResponse>, Status> { Err(Status::unimplemented("bake_macaroon")) }
    async fn list_macaroon_i_ds(&self, _: Request<ListMacaroonIDsRequest>) -> Result<Response<ListMacaroonIDsResponse>, Status> { Err(Status::unimplemented("list_macaroon_i_ds")) }
    async fn delete_macaroon_id(&self, _: Request<DeleteMacaroonIdRequest>) -> Result<Response<DeleteMacaroonIdResponse>, Status> { Err(Status::unimplemented("delete_macaroon_id")) }
    async fn list_permissions(&self, _: Request<ListPermissionsRequest>) -> Result<Response<ListPermissionsResponse>, Status> { Err(Status::unimplemented("list_permissions")) }
    async fn check_macaroon_permissions(&self, _: Request<CheckMacPermRequest>) -> Result<Response<CheckMacPermResponse>, Status> { Err(Status::unimplemented("check_macaroon_permissions")) }
    async fn send_custom_message(&self, _: Request<SendCustomMessageRequest>) -> Result<Response<SendCustomMessageResponse>, Status> { Err(Status::unimplemented("send_custom_message")) }
    async fn list_aliases(&self, _: Request<ListAliasesRequest>) -> Result<Response<ListAliasesResponse>, Status> { Err(Status::unimplemented("list_aliases")) }
    async fn lookup_htlc_resolution(&self, _: Request<LookupHtlcResolutionRequest>) -> Result<Response<LookupHtlcResolutionResponse>, Status> { Err(Status::unimplemented("lookup_htlc_resolution")) }

    type SubscribeTransactionsStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<Transaction, Status>> + Send>>;
    type SubscribePeerEventsStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<PeerEvent, Status>> + Send>>;
    type SubscribeChannelEventsStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChannelEventUpdate, Status>> + Send>>;
    type OpenChannelStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<OpenStatusUpdate, Status>> + Send>>;
    type CloseChannelStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<CloseStatusUpdate, Status>> + Send>>;
    type SendPaymentStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<SendResponse, Status>> + Send>>;
    type SendToRouteStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<SendResponse, Status>> + Send>>;
    type SubscribeInvoicesStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<Invoice, Status>> + Send>>;
    type SubscribeChannelGraphStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<GraphTopologyUpdate, Status>> + Send>>;
    type SubscribeChannelBackupsStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChanBackupSnapshot, Status>> + Send>>;
    type SubscribeCustomMessagesStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<CustomMessage, Status>> + Send>>;
    type ChannelAcceptorStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<ChannelAcceptRequest, Status>> + Send>>;
    type RegisterRPCMiddlewareStream = std::pin::Pin<Box<dyn futures::Stream<Item = Result<RpcMiddlewareRequest, Status>> + Send>>;
    
    async fn subscribe_transactions(&self, _: Request<GetTransactionsRequest>) -> Result<Response<Self::SubscribeTransactionsStream>, Status> { Err(Status::unimplemented("subscribe_transactions")) }
    async fn subscribe_peer_events(&self, _: Request<PeerEventSubscription>) -> Result<Response<Self::SubscribePeerEventsStream>, Status> { Err(Status::unimplemented("subscribe_peer_events")) }
    async fn subscribe_channel_events(&self, _: Request<ChannelEventSubscription>) -> Result<Response<Self::SubscribeChannelEventsStream>, Status> { Err(Status::unimplemented("subscribe_channel_events")) }
    async fn open_channel(&self, _: Request<OpenChannelRequest>) -> Result<Response<Self::OpenChannelStream>, Status> { Err(Status::unimplemented("open_channel")) }
    async fn close_channel(&self, _: Request<CloseChannelRequest>) -> Result<Response<Self::CloseChannelStream>, Status> { Err(Status::unimplemented("close_channel")) }
    async fn send_payment(&self, _: Request<Streaming<SendRequest>>) -> Result<Response<Self::SendPaymentStream>, Status> { Err(Status::unimplemented("send_payment")) }
    async fn send_to_route(&self, _: Request<Streaming<SendToRouteRequest>>) -> Result<Response<Self::SendToRouteStream>, Status> { Err(Status::unimplemented("send_to_route")) }
    async fn subscribe_invoices(&self, _: Request<InvoiceSubscription>) -> Result<Response<Self::SubscribeInvoicesStream>, Status> { Err(Status::unimplemented("subscribe_invoices")) }
    async fn subscribe_channel_graph(&self, _: Request<GraphTopologySubscription>) -> Result<Response<Self::SubscribeChannelGraphStream>, Status> { Err(Status::unimplemented("subscribe_channel_graph")) }
    async fn subscribe_channel_backups(&self, _: Request<ChannelBackupSubscription>) -> Result<Response<Self::SubscribeChannelBackupsStream>, Status> { Err(Status::unimplemented("subscribe_channel_backups")) }
    async fn subscribe_custom_messages(&self, _: Request<SubscribeCustomMessagesRequest>) -> Result<Response<Self::SubscribeCustomMessagesStream>, Status> { Err(Status::unimplemented("subscribe_custom_messages")) }
    async fn channel_acceptor(&self, _: Request<Streaming<ChannelAcceptResponse>>) -> Result<Response<Self::ChannelAcceptorStream>, Status> { Err(Status::unimplemented("channel_acceptor")) }
    async fn register_rpc_middleware(&self, _: Request<Streaming<RpcMiddlewareResponse>>) -> Result<Response<Self::RegisterRPCMiddlewareStream>, Status> { Err(Status::unimplemented("register_rpc_middleware")) }
}