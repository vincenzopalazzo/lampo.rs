//! HTTP route handlers matching LND grpc-gateway paths.
use actix_web::{delete, get, post, web, HttpRequest, HttpResponse};
use bytes::Bytes;
use lampo_common::json;
use lampo_common::model::request;
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::auth::{
    AuthError, ADDRESS_WRITE, INFO_READ, INVOICES_READ, INVOICES_WRITE, OFFCHAIN_READ,
    OFFCHAIN_WRITE, ONCHAIN_READ, PEERS_READ, PEERS_WRITE,
};
use crate::convert;
use crate::lnrpc;
use crate::routes::{authorize, AppState};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(get_info)
        .service(wallet_balance)
        .service(channel_balance)
        .service(list_channels)
        .service(pending_channels)
        .service(closed_channels)
        .service(list_peers)
        .service(connect_peer)
        .service(new_address)
        .service(add_invoice)
        .service(lookup_invoice)
        .service(list_invoices)
        .service(decode_payreq)
        .service(send_payment_sync)
        .service(router_send)
        .service(open_channel)
        .service(close_channel)
        .service(list_payments)
        .service(list_transactions);
}

fn auth_error(err: AuthError) -> HttpResponse {
    let (status, message) = match err {
        AuthError::Missing
        | AuthError::Malformed
        | AuthError::InvalidSignature
        | AuthError::TooLarge => (actix_web::http::StatusCode::UNAUTHORIZED, err.to_string()),
        AuthError::PermissionDenied | AuthError::UnknownCaveat(_) => {
            (actix_web::http::StatusCode::FORBIDDEN, err.to_string())
        }
        AuthError::Io(_) | AuthError::Other(_) => (
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
            err.to_string(),
        ),
    };
    HttpResponse::build(status).json(json::json!({
        "code": status.as_u16(),
        "message": message,
        "details": []
    }))
}

fn internal_error(err: impl std::fmt::Debug) -> HttpResponse {
    log::error!(target: "lampo-lnd", "handler error: {:?}", err);
    HttpResponse::InternalServerError().json(json::json!({
        "code": 500,
        "message": format!("{:?}", err),
        "details": []
    }))
}

fn proto_json<T: serde::Serialize>(msg: &T) -> HttpResponse {
    match serde_json::to_value(msg) {
        Ok(v) => HttpResponse::Ok().json(v),
        Err(e) => internal_error(e),
    }
}

#[get("/v1/getinfo")]
async fn get_info(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, INFO_READ) {
        return auth_error(e);
    }
    let lampod = &state.lampod;
    let (block_hash, height) = match lampod.onchain_manager().backend.get_best_block().await {
        Ok(v) => v,
        Err(e) => return internal_error(e),
    };
    let node_id = lampod
        .channel_manager()
        .manager()
        .get_our_node_id()
        .to_string();
    let alias = lampod.conf().alias.clone().unwrap_or_default();
    let network = convert::chain_network(&lampod.conf().network.to_string());
    let mut uris = Vec::new();
    if let Some(addr) = lampod.conf().announce_addr.clone() {
        uris.push(format!("{}@{}:{}", node_id, addr, lampod.conf().port));
    }

    let info = lnrpc::GetInfoResponse {
        version: "0.18.5-beta".to_string(),
        commit_hash: String::new(),
        identity_pubkey: node_id,
        alias,
        color: "#3399ff".to_string(),
        num_pending_channels: 0,
        num_active_channels: lampod.channel_manager().list_channels().channels.len() as u32,
        num_inactive_channels: 0,
        num_peers: lampod.peer_manager().manager().list_peers().len() as u32,
        block_height: height.unwrap_or_default(),
        block_hash: block_hash.to_string(),
        best_header_timestamp: 0,
        synced_to_chain: true,
        synced_to_graph: true,
        chains: vec![lnrpc::Chain {
            network,
            ..Default::default()
        }],
        uris,
        features: Default::default(),
        ..Default::default()
    };
    proto_json(&info)
}

#[get("/v1/balance/blockchain")]
async fn wallet_balance(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, ONCHAIN_READ) {
        return auth_error(e);
    }
    match state.lampod.wallet_manager().get_onchain_balance().await {
        Ok(confirmed) => {
            let confirmed = confirmed as i64;
            let resp = lnrpc::WalletBalanceResponse {
                total_balance: confirmed,
                confirmed_balance: confirmed,
                unconfirmed_balance: 0,
                locked_balance: 0,
                reserved_balance_anchor_chan: 0,
                account_balance: Default::default(),
            };
            proto_json(&resp)
        }
        Err(e) => internal_error(e),
    }
}

#[get("/v1/balance/channels")]
async fn channel_balance(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, OFFCHAIN_READ) {
        return auth_error(e);
    }
    let channels = state.lampod.channel_manager().list_channels();
    let local: i64 = channels
        .channels
        .iter()
        .map(|c| (c.available_balance_for_send_msat / 1000) as i64)
        .sum();
    let remote: i64 = channels
        .channels
        .iter()
        .map(|c| (c.available_balance_for_recv_msat / 1000) as i64)
        .sum();
    let resp = lnrpc::ChannelBalanceResponse {
        local_balance: Some(lnrpc::Amount {
            sat: local as u64,
            msat: (local as u64) * 1000,
        }),
        remote_balance: Some(lnrpc::Amount {
            sat: remote as u64,
            msat: (remote as u64) * 1000,
        }),
        unsettled_local_balance: Some(lnrpc::Amount::default()),
        unsettled_remote_balance: Some(lnrpc::Amount::default()),
        pending_open_local_balance: Some(lnrpc::Amount::default()),
        pending_open_remote_balance: Some(lnrpc::Amount::default()),
        custom_channel_data: Bytes::new(),
        ..Default::default()
    };
    proto_json(&resp)
}

#[get("/v1/channels")]
async fn list_channels(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, OFFCHAIN_READ) {
        return auth_error(e);
    }
    let list: Vec<lnrpc::Channel> = state
        .lampod
        .channel_manager()
        .manager()
        .list_channels()
        .into_iter()
        .map(|c| {
            let channel_point = c
                .funding_txo
                .map(|o| format!("{}:{}", o.txid, o.index))
                .unwrap_or_default();
            lnrpc::Channel {
                active: c.is_channel_ready,
                remote_pubkey: c.counterparty.node_id.to_string(),
                channel_point,
                chan_id: c.short_channel_id.unwrap_or_default(),
                capacity: c.channel_value_satoshis as i64,
                local_balance: (c.outbound_capacity_msat / 1000) as i64,
                remote_balance: (c.inbound_capacity_msat / 1000) as i64,
                private: !c.is_announced,
                ..Default::default()
            }
        })
        .collect();
    proto_json(&lnrpc::ListChannelsResponse { channels: list })
}

#[get("/v1/channels/pending")]
async fn pending_channels(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, OFFCHAIN_READ) {
        return auth_error(e);
    }
    proto_json(&lnrpc::PendingChannelsResponse::default())
}

#[get("/v1/channels/closed")]
async fn closed_channels(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, OFFCHAIN_READ) {
        return auth_error(e);
    }
    proto_json(&lnrpc::ClosedChannelsResponse::default())
}

#[get("/v1/peers")]
async fn list_peers(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, PEERS_READ) {
        return auth_error(e);
    }
    let peers = state
        .lampod
        .peer_manager()
        .manager()
        .list_peers()
        .into_iter()
        .map(|p| lnrpc::Peer {
            pub_key: p.counterparty_node_id.to_string(),
            address: String::new(),
            bytes_sent: 0,
            bytes_recv: 0,
            sat_sent: 0,
            sat_recv: 0,
            inbound: false,
            ping_time: 0,
            sync_type: 0,
            features: Default::default(),
            errors: Vec::new(),
            flap_count: 0,
            last_flap_ns: 0,
            last_ping_payload: Bytes::new(),
        })
        .collect();
    proto_json(&lnrpc::ListPeersResponse { peers })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectPeerBody {
    addr: Option<ConnectAddr>,
    #[serde(default)]
    #[allow(dead_code)]
    perm: bool,
    #[serde(default)]
    #[allow(dead_code)]
    timeout: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectAddr {
    pubkey: Option<String>,
    host: Option<String>,
}

#[post("/v1/peers")]
async fn connect_peer(
    req: HttpRequest,
    state: web::Data<AppState>,
    body: web::Json<JsonValue>,
) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, PEERS_WRITE) {
        return auth_error(e);
    }
    let parsed: ConnectPeerBody = match serde_json::from_value(body.into_inner()) {
        Ok(v) => v,
        Err(e) => return internal_error(e),
    };
    let Some(addr) = parsed.addr else {
        return internal_error("missing addr");
    };
    let (pubkey, host) = match (addr.pubkey, addr.host) {
        (Some(p), Some(h)) => (p, h),
        _ => return internal_error("addr.pubkey and addr.host are required"),
    };
    // host may be "ip:port" or "ip"
    let (ip, port) = if let Some((h, p)) = host.rsplit_once(':') {
        (h.to_string(), p.parse::<u64>().unwrap_or(9735))
    } else {
        (host, 9735)
    };
    let request = request::Connect {
        node_id: pubkey,
        addr: ip,
        port,
    };
    match lampod::jsonrpc::peer_control::json_connect(
        &state.lampod,
        &json::to_value(request).unwrap_or_default(),
    )
    .await
    {
        Ok(_) => proto_json(&lnrpc::ConnectPeerResponse::default()),
        Err(e) => internal_error(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewAddressBody {
    #[serde(default)]
    #[allow(dead_code)]
    r#type: i32,
    #[serde(default)]
    #[allow(dead_code)]
    account: String,
}

#[post("/v1/newaddress")]
async fn new_address(
    req: HttpRequest,
    state: web::Data<AppState>,
    _body: web::Json<NewAddressBody>,
) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, ADDRESS_WRITE) {
        return auth_error(e);
    }
    match state.lampod.wallet_manager().get_onchain_address().await {
        Ok(addr) => proto_json(&lnrpc::NewAddressResponse {
            address: addr.address,
        }),
        Err(e) => internal_error(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddInvoiceBody {
    #[serde(default)]
    memo: String,
    #[serde(default)]
    value: i64,
    #[serde(default)]
    value_msat: i64,
    #[serde(default)]
    expiry: i64,
}

#[post("/v1/invoices")]
async fn add_invoice(
    req: HttpRequest,
    state: web::Data<AppState>,
    body: web::Json<AddInvoiceBody>,
) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, INVOICES_WRITE) {
        return auth_error(e);
    }
    let amount_msat = if body.value_msat > 0 {
        Some(body.value_msat as u64)
    } else if body.value > 0 {
        Some((body.value as u64) * 1000)
    } else {
        None
    };
    let expiry = if body.expiry > 0 {
        body.expiry as u32
    } else {
        3600
    };
    let description = if body.memo.is_empty() {
        "lampo invoice".to_string()
    } else {
        body.memo.clone()
    };
    let invoice =
        match state
            .lampod
            .offchain_manager()
            .generate_invoice(amount_msat, &description, expiry)
        {
            Ok(inv) => inv,
            Err(e) => return internal_error(e),
        };

    let r_hash_hex = hex::encode(invoice.payment_hash().0);
    let r_hash = Bytes::from(invoice.payment_hash().0.to_vec());

    let lnd_invoice = lnrpc::Invoice {
        memo: description,
        r_preimage: Bytes::new(),
        r_hash: r_hash.clone(),
        value: amount_msat.map(|v| (v / 1000) as i64).unwrap_or(0),
        value_msat: amount_msat.unwrap_or(0) as i64,
        payment_request: invoice.to_string(),
        expiry: expiry as i64,
        state: lnrpc::invoice::InvoiceState::Open as i32,
        ..Default::default()
    };

    state
        .invoices
        .write()
        .await
        .insert(r_hash_hex, lnd_invoice.clone());

    proto_json(&lnrpc::AddInvoiceResponse {
        r_hash,
        payment_request: invoice.to_string(),
        add_index: 0,
        payment_addr: Bytes::new(),
    })
}

#[get("/v1/invoice/{r_hash}")]
async fn lookup_invoice(
    req: HttpRequest,
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, INVOICES_READ) {
        return auth_error(e);
    }
    let raw = path.into_inner();
    let key = convert::normalize_r_hash(&raw);
    let index = state.invoices.read().await;
    let found = key
        .as_ref()
        .and_then(|k| index.get(k))
        .or_else(|| index.get(&raw));
    match found {
        Some(inv) => proto_json(&inv),
        None => HttpResponse::NotFound().json(json::json!({
            "code": 5,
            "message": "unable to locate invoice",
            "details": []
        })),
    }
}

#[get("/v1/invoices")]
async fn list_invoices(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, INVOICES_READ) {
        return auth_error(e);
    }
    let invoices = state.invoices.read().await.list();
    proto_json(&lnrpc::ListInvoiceResponse {
        invoices,
        last_index_offset: 0,
        first_index_offset: 0,
    })
}

#[get("/v1/payreq/{payreq}")]
async fn decode_payreq(
    req: HttpRequest,
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, INFO_READ) {
        return auth_error(e);
    }
    let payreq = path.into_inner();
    let request = request::DecodeInvoice {
        invoice_str: payreq,
    };
    match lampod::jsonrpc::offchain::json_decode(
        &state.lampod,
        &json::to_value(request).unwrap_or_default(),
    )
    .await
    {
        Ok(value) => {
            // Best-effort map into PayReq.
            let amount_msat = value
                .get("amount_msat")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let destination = value
                .get("issuer_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let description = value
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let expiry = value
                .get("expiry_time")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            proto_json(&lnrpc::PayReq {
                destination,
                payment_hash: String::new(),
                num_satoshis: (amount_msat / 1000) as i64,
                timestamp: 0,
                expiry: expiry as i64,
                description,
                description_hash: String::new(),
                fallback_addr: String::new(),
                cltv_expiry: 0,
                route_hints: Vec::new(),
                payment_addr: Bytes::new(),
                num_msat: amount_msat as i64,
                features: Default::default(),
                blinded_paths: Vec::new(),
            })
        }
        Err(e) => internal_error(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendPaymentBody {
    #[serde(default, alias = "payment_request")]
    payment_request: String,
    #[serde(default)]
    amt: i64,
    #[serde(default)]
    amt_msat: i64,
}

async fn pay_invoice(
    state: &AppState,
    body: &SendPaymentBody,
) -> Result<lnrpc::SendResponse, String> {
    if body.payment_request.is_empty() {
        return Err("payment_request is required".into());
    }
    let amount_msat = if body.amt_msat > 0 {
        Some(body.amt_msat as u64)
    } else if body.amt > 0 {
        Some((body.amt as u64) * 1000)
    } else {
        None
    };
    let request = request::Pay {
        invoice_str: body.payment_request.clone(),
        amount: amount_msat,
        bolt12: None,
    };
    let value = lampod::jsonrpc::offchain::json_pay(
        &state.lampod,
        &json::to_value(request).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;

    let state_str = value
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("Failure");
    let payment_hash = value
        .get("payment_hash")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let payment_preimage = value
        .get("payment_preimage")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if state_str != "Success" {
        return Err(format!("payment failed with state {state_str}"));
    }

    Ok(lnrpc::SendResponse {
        payment_error: String::new(),
        payment_preimage: Bytes::from(hex::decode(payment_preimage).unwrap_or_default()),
        payment_hash: Bytes::from(hex::decode(payment_hash).unwrap_or_default()),
        payment_route: None,
        ..Default::default()
    })
}

#[post("/v1/channels/transactions")]
async fn send_payment_sync(
    req: HttpRequest,
    state: web::Data<AppState>,
    body: web::Json<SendPaymentBody>,
) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, OFFCHAIN_WRITE) {
        return auth_error(e);
    }
    match pay_invoice(&state, &body).await {
        Ok(resp) => proto_json(&resp),
        Err(e) => internal_error(e),
    }
}

/// Zeus pays via `/v2/router/send` expecting NDJSON `{ "result": ... }` chunks.
#[post("/v2/router/send")]
async fn router_send(
    req: HttpRequest,
    state: web::Data<AppState>,
    body: web::Json<SendPaymentBody>,
) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, OFFCHAIN_WRITE) {
        return auth_error(e);
    }
    match pay_invoice(&state, &body).await {
        Ok(resp) => {
            use base64::Engine;
            let engine = base64::engine::general_purpose::STANDARD;
            let status = json::json!({
                "result": {
                    "status": "SUCCEEDED",
                    "paymentHash": engine.encode(&resp.payment_hash),
                    "paymentPreimage": engine.encode(&resp.payment_preimage),
                    "feeMsat": "0",
                    "valueMsat": "0"
                }
            });
            let line = format!("{}\n", status);
            HttpResponse::Ok()
                .content_type("application/json")
                .body(line)
        }
        Err(e) => {
            let status = json::json!({
                "result": {
                    "status": "FAILED",
                    "failureReason": "FAILURE_REASON_NO_ROUTE",
                    "paymentError": e
                }
            });
            let line = format!("{}\n", status);
            HttpResponse::Ok()
                .content_type("application/json")
                .body(line)
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenChannelBody {
    #[serde(default, alias = "node_pubkey_string")]
    node_pubkey_string: String,
    #[serde(default, alias = "node_pubkey")]
    node_pubkey: String,
    #[serde(default, alias = "local_funding_amount")]
    local_funding_amount: i64,
    #[serde(default)]
    private: bool,
}

#[post("/v1/channels")]
async fn open_channel(
    req: HttpRequest,
    state: web::Data<AppState>,
    body: web::Json<OpenChannelBody>,
) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, OFFCHAIN_WRITE) {
        return auth_error(e);
    }
    let node_id = if !body.node_pubkey_string.is_empty() {
        body.node_pubkey_string.clone()
    } else {
        body.node_pubkey.clone()
    };
    if node_id.is_empty() || body.local_funding_amount <= 0 {
        return internal_error("node_pubkey_string and local_funding_amount are required");
    }
    let request = request::OpenChannel {
        node_id,
        amount: body.local_funding_amount as u64,
        public: !body.private,
        port: None,
        addr: None,
    };
    match lampod::jsonrpc::open_channel::json_fundchannel(
        &state.lampod,
        &json::to_value(request).unwrap_or_default(),
    )
    .await
    {
        Ok(value) => {
            let txid = value
                .get("tx")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            proto_json(&lnrpc::ChannelPoint {
                funding_txid: Some(lnrpc::channel_point::FundingTxid::FundingTxidStr(txid)),
                output_index: 0,
            })
        }
        Err(e) => internal_error(e),
    }
}

#[delete("/v1/channels/{funding_txid}/{output_index}")]
async fn close_channel(
    req: HttpRequest,
    state: web::Data<AppState>,
    path: web::Path<(String, u32)>,
) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, OFFCHAIN_WRITE) {
        return auth_error(e);
    }
    let (funding_txid, output_index) = path.into_inner();
    // Resolve channel by funding outpoint against LDK channel details.
    let details = state.lampod.channel_manager().manager().list_channels();
    let Some(channel) = details.into_iter().find(|c| {
        c.funding_txo
            .map(|o| o.txid.to_string() == funding_txid && o.index as u32 == output_index)
            .unwrap_or(false)
    }) else {
        return HttpResponse::NotFound().json(json::json!({
            "code": 5,
            "message": "channel not found",
            "details": []
        }));
    };

    let request = request::CloseChannel {
        node_id: channel.counterparty.node_id.to_string(),
        channel_id: Some(channel.channel_id.to_string()),
    };
    match lampod::jsonrpc::channels::json_close(
        &state.lampod,
        &json::to_value(request).unwrap_or_default(),
    )
    .await
    {
        Ok(_) => HttpResponse::Ok().json(json::json!({})),
        Err(e) => internal_error(e),
    }
}

#[get("/v1/payments")]
async fn list_payments(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, OFFCHAIN_READ) {
        return auth_error(e);
    }
    proto_json(&lnrpc::ListPaymentsResponse::default())
}

#[get("/v1/transactions")]
async fn list_transactions(req: HttpRequest, state: web::Data<AppState>) -> HttpResponse {
    if let Err(e) = authorize(&req, &state.bakery, ONCHAIN_READ) {
        return auth_error(e);
    }
    proto_json(&lnrpc::TransactionDetails::default())
}
