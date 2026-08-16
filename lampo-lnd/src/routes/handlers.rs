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
    let channels = lampod.channel_manager().manager().list_channels();
    let num_pending_channels = channels.iter().filter(|c| !c.is_channel_ready).count() as u32;
    let num_active_channels = channels.iter().filter(|c| c.is_usable).count() as u32;
    let num_inactive_channels = channels
        .iter()
        .filter(|c| c.is_channel_ready && !c.is_usable)
        .count() as u32;
    let synced_to_chain = match (height, lampod.wallet_manager().wallet_tips().await) {
        (Some(best_height), Ok(wallet_height)) => wallet_height.to_consensus_u32() >= best_height,
        _ => false,
    };
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
        num_pending_channels,
        num_active_channels,
        num_inactive_channels,
        num_peers: lampod.peer_manager().manager().list_peers().len() as u32,
        block_height: height.unwrap_or_default(),
        block_hash: block_hash.to_string(),
        best_header_timestamp: 0,
        synced_to_chain,
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
                active: c.is_usable,
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

fn parse_peer_host(host: String) -> Result<(String, u64), String> {
    if let Ok(socket) = host.parse::<std::net::SocketAddr>() {
        return Ok((socket.ip().to_string(), socket.port() as u64));
    }
    if host.starts_with('[') {
        return Err("invalid bracketed peer address".into());
    }
    if host.matches(':').count() == 1 {
        let (hostname, port) = host
            .split_once(':')
            .ok_or_else(|| "invalid peer address".to_string())?;
        let port = port
            .parse::<u16>()
            .map_err(|_| "invalid peer port".to_string())?;
        if hostname.is_empty() || port == 0 {
            return Err("invalid peer address".into());
        }
        return Ok((hostname.to_string(), port as u64));
    }
    // A hostname/IPv4 address, or an unbracketed IPv6 address without a port.
    Ok((host, 9735))
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
    let (ip, port) = match parse_peer_host(host) {
        Ok(addr) => addr,
        Err(e) => return internal_error(e),
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
            let payment_hash = value
                .get("payment_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            proto_json(&lnrpc::PayReq {
                destination,
                payment_hash,
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
    #[serde(
        default,
        alias = "fee_limit_sat",
        deserialize_with = "deserialize_optional_i64"
    )]
    fee_limit_sat: Option<i64>,
    #[serde(
        default,
        alias = "fee_limit_msat",
        deserialize_with = "deserialize_optional_i64"
    )]
    fee_limit_msat: Option<i64>,
    #[serde(default)]
    fee_limit: Option<FeeLimitBody>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeeLimitBody {
    #[serde(default, deserialize_with = "deserialize_optional_i64")]
    fixed: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_i64")]
    fixed_msat: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_i64")]
    percent: Option<i64>,
}

fn deserialize_optional_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum I64Value {
        Number(i64),
        String(String),
    }

    match Option::<I64Value>::deserialize(deserializer)? {
        Some(I64Value::Number(value)) => Ok(Some(value)),
        Some(I64Value::String(value)) => value.parse().map(Some).map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

fn deserialize_i64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_optional_i64(deserializer).map(Option::unwrap_or_default)
}

#[derive(Clone, Copy)]
enum PaymentEndpoint {
    Sync,
    Router,
}

struct PaymentOutcome {
    response: lnrpc::SendResponse,
    value_msat: u64,
    fee_msat: u64,
}

fn max_fee_msat(
    body: &SendPaymentBody,
    value_msat: u64,
    endpoint: PaymentEndpoint,
) -> Result<u64, String> {
    if let Some(fee_limit) = &body.fee_limit {
        if body.fee_limit_sat.is_some() || body.fee_limit_msat.is_some() {
            return Err("flat and nested fee limits are mutually exclusive".into());
        }
        let limits = [
            fee_limit.fixed.map(|value| (value, 1000_u64)),
            fee_limit.fixed_msat.map(|value| (value, 1_u64)),
            fee_limit.percent.map(|value| (value, 0_u64)),
        ];
        if limits.iter().filter(|limit| limit.is_some()).count() != 1 {
            return Err("feeLimit must specify exactly one limit".into());
        }
        let (value, multiplier) = limits
            .into_iter()
            .flatten()
            .next()
            .ok_or_else(|| "feeLimit is empty".to_string())?;
        if value < 0 {
            return Err("fee limits must not be negative".into());
        }
        if multiplier == 0 {
            return value_msat
                .checked_mul(value as u64)
                .map(|fee| fee / 100)
                .ok_or_else(|| "feeLimit.percent is too large".to_string());
        }
        return (value as u64)
            .checked_mul(multiplier)
            .ok_or_else(|| "fixed fee limit is too large".to_string());
    }

    if body.fee_limit_sat.is_some() && body.fee_limit_msat.is_some() {
        return Err("fee_limit_sat and fee_limit_msat are mutually exclusive".into());
    }
    if let Some(value) = body.fee_limit_msat {
        return u64::try_from(value).map_err(|_| "fee limits must not be negative".into());
    }
    if let Some(value) = body.fee_limit_sat {
        return u64::try_from(value)
            .map_err(|_| "fee limits must not be negative".to_string())?
            .checked_mul(1000)
            .ok_or_else(|| "fee_limit_sat is too large".to_string());
    }

    match endpoint {
        PaymentEndpoint::Router => Ok(0),
        PaymentEndpoint::Sync if value_msat <= 1_000_000 => Ok(value_msat),
        PaymentEndpoint::Sync => Ok(value_msat / 20),
    }
}

fn route_value_and_fee(value: &JsonValue, fallback_value_msat: u64) -> (u64, u64) {
    let path_hop_fees = value
        .get("path")
        .and_then(|v| v.as_array())
        .map(|path| {
            path.iter()
                .filter_map(|hop| hop.get("hop_fee_msat").and_then(|fee| fee.as_u64()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // LDK stores the delivered value in the final non-blinded RouteHop's
    // `fee_msat`; preceding hops contain routing fees.
    let routed_value_msat = path_hop_fees.last().copied();
    let total_hop_msat: u64 = path_hop_fees.iter().sum();
    (
        routed_value_msat.unwrap_or(fallback_value_msat),
        total_hop_msat.saturating_sub(routed_value_msat.unwrap_or(0)),
    )
}

async fn pay_invoice(
    state: &AppState,
    body: &SendPaymentBody,
    endpoint: PaymentEndpoint,
) -> Result<PaymentOutcome, String> {
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
    let invoice_amount_msat = state
        .lampod
        .offchain_manager()
        .decode_invoice(&body.payment_request)
        .ok()
        .and_then(|invoice| invoice.amount_milli_satoshis());
    let value_msat = invoice_amount_msat.or(amount_msat).unwrap_or(0);
    let max_fee_msat = max_fee_msat(body, value_msat, endpoint)?;
    let request = request::Pay {
        invoice_str: body.payment_request.clone(),
        amount: amount_msat,
        max_fee_msat: Some(max_fee_msat),
        bolt12: None,
        timeout: Default::default(),
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
    let (value_msat, fee_msat) = route_value_and_fee(&value, value_msat);

    if state_str != "Success" {
        return Err(format!("payment failed with state {state_str}"));
    }

    Ok(PaymentOutcome {
        response: lnrpc::SendResponse {
            payment_error: String::new(),
            payment_preimage: Bytes::from(hex::decode(payment_preimage).unwrap_or_default()),
            payment_hash: Bytes::from(hex::decode(payment_hash).unwrap_or_default()),
            payment_route: Some(lnrpc::Route {
                total_fees: (fee_msat / 1000) as i64,
                total_amt: (value_msat.saturating_add(fee_msat) / 1000) as i64,
                total_fees_msat: fee_msat as i64,
                total_amt_msat: value_msat.saturating_add(fee_msat) as i64,
                ..Default::default()
            }),
            ..Default::default()
        },
        value_msat,
        fee_msat,
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
    match pay_invoice(&state, &body, PaymentEndpoint::Sync).await {
        Ok(outcome) => proto_json(&outcome.response),
        Err(e) => proto_json(&lnrpc::SendResponse {
            payment_error: e,
            ..Default::default()
        }),
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
    match pay_invoice(&state, &body, PaymentEndpoint::Router).await {
        Ok(outcome) => {
            use base64::Engine;
            let engine = base64::engine::general_purpose::STANDARD;
            let status = json::json!({
                "result": {
                    "status": "SUCCEEDED",
                    "paymentHash": engine.encode(&outcome.response.payment_hash),
                    "paymentPreimage": engine.encode(&outcome.response.payment_preimage),
                    "feeMsat": outcome.fee_msat.to_string(),
                    "valueMsat": outcome.value_msat.to_string()
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
    #[serde(
        default,
        alias = "local_funding_amount",
        deserialize_with = "deserialize_i64"
    )]
    local_funding_amount: i64,
    #[serde(
        default,
        alias = "push_sat",
        deserialize_with = "deserialize_optional_i64"
    )]
    push_sat: Option<i64>,
    #[serde(default)]
    private: bool,
}

fn open_channel_node_id(body: &OpenChannelBody) -> Result<String, String> {
    if !body.node_pubkey_string.is_empty() {
        return Ok(body.node_pubkey_string.clone());
    }
    if body.node_pubkey.is_empty() {
        return Err("nodePubkey or nodePubkeyString is required".into());
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&body.node_pubkey)
        .map_err(|_| "nodePubkey must be base64 encoded".to_string())?;
    if bytes.len() != 33 {
        return Err("nodePubkey must encode a 33-byte public key".into());
    }
    lampo_common::secp256k1::PublicKey::from_slice(&bytes)
        .map(|public_key| public_key.to_string())
        .map_err(|_| "nodePubkey is not a valid compressed public key".into())
}

fn open_channel_push_msat(body: &OpenChannelBody) -> Result<Option<u64>, String> {
    let Some(push_sat) = body.push_sat else {
        return Ok(None);
    };
    if push_sat < 0 || push_sat > body.local_funding_amount {
        return Err("pushSat must be between zero and localFundingAmount".into());
    }
    u64::try_from(push_sat)
        .map_err(|_| "pushSat must not be negative".to_string())?
        .checked_mul(1000)
        .map(Some)
        .ok_or_else(|| "pushSat is too large".to_string())
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
    let node_id = match open_channel_node_id(&body) {
        Ok(node_id) => node_id,
        Err(e) => return internal_error(e),
    };
    if body.local_funding_amount <= 0 {
        return internal_error("local_funding_amount is required");
    }
    let push_msat = match open_channel_push_msat(&body) {
        Ok(push_msat) => push_msat,
        Err(e) => return internal_error(e),
    };
    let expected_node_id = node_id.clone();
    let request = request::OpenChannel {
        node_id,
        amount: body.local_funding_amount as u64,
        public: !body.private,
        port: None,
        addr: None,
        push_msat,
    };
    match lampod::jsonrpc::open_channel::json_fundchannel(
        &state.lampod,
        &json::to_value(request).unwrap_or_default(),
    )
    .await
    {
        Ok(value) => {
            let txid = value
                .get("txid")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let output_index = state
                .lampod
                .channel_manager()
                .manager()
                .list_channels()
                .into_iter()
                .filter(|channel| channel.counterparty.node_id.to_string() == expected_node_id)
                .filter_map(|channel| channel.funding_txo)
                .find(|outpoint| outpoint.txid.to_string() == txid)
                .map(|outpoint| outpoint.index as u32);
            let Some(output_index) = output_index else {
                return internal_error("funding output index is not available");
            };
            proto_json(&lnrpc::ChannelPoint {
                funding_txid: Some(lnrpc::channel_point::FundingTxid::FundingTxidStr(txid)),
                output_index,
            })
        }
        Err(e) => internal_error(e),
    }
}

#[derive(Debug, Default, Deserialize)]
struct CloseChannelQuery {
    #[serde(default)]
    force: bool,
}

#[delete("/v1/channels/{funding_txid}/{output_index}")]
async fn close_channel(
    req: HttpRequest,
    state: web::Data<AppState>,
    path: web::Path<(String, u32)>,
    query: web::Query<CloseChannelQuery>,
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
        force: query.force,
    };
    match lampod::jsonrpc::channels::json_close(
        &state.lampod,
        &json::to_value(request).unwrap_or_default(),
    )
    .await
    {
        Ok(_) => {
            let update = lnrpc::CloseStatusUpdate {
                update: Some(lnrpc::close_status_update::Update::ChanClose(
                    lnrpc::ChannelCloseUpdate {
                        success: true,
                        ..Default::default()
                    },
                )),
            };
            match serde_json::to_value(update) {
                Ok(update) => HttpResponse::Ok()
                    .content_type("application/json")
                    .body(format!("{}\n", json::json!({ "result": update }))),
                Err(e) => internal_error(e),
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_zeus_fee_limit_and_payment_request() {
        let body: SendPaymentBody = serde_json::from_value(json::json!({
            "paymentRequest": "lnbc1...",
            "feeLimitMsat": 1234
        }))
        .unwrap();
        assert_eq!(body.payment_request, "lnbc1...");
        assert_eq!(
            max_fee_msat(&body, 100_000, PaymentEndpoint::Router).unwrap(),
            1234
        );
    }

    #[test]
    fn endpoint_fee_defaults_match_lnd() {
        let body: SendPaymentBody =
            serde_json::from_value(json::json!({ "paymentRequest": "lnbc1..." })).unwrap();
        assert_eq!(
            max_fee_msat(&body, 2_000_000, PaymentEndpoint::Router).unwrap(),
            0
        );
        assert_eq!(
            max_fee_msat(&body, 2_000_000, PaymentEndpoint::Sync).unwrap(),
            100_000
        );
    }

    #[test]
    fn parses_nested_synchronous_fee_limit() {
        let body: SendPaymentBody = serde_json::from_value(json::json!({
            "paymentRequest": "lnbc1...",
            "feeLimit": { "fixedMsat": "4321" }
        }))
        .unwrap();
        assert_eq!(
            max_fee_msat(&body, 100_000, PaymentEndpoint::Sync).unwrap(),
            4321
        );
    }

    #[test]
    fn parses_peer_hostname_and_ipv6() {
        assert_eq!(
            parse_peer_host("node.example.com:19735".into()).unwrap(),
            ("node.example.com".into(), 19735)
        );
        assert_eq!(
            parse_peer_host("[::1]:9736".into()).unwrap(),
            ("::1".into(), 9736)
        );
        assert_eq!(parse_peer_host("::1".into()).unwrap(), ("::1".into(), 9735));
    }

    #[test]
    fn decodes_rest_node_pubkey() {
        use base64::Engine;
        use lampo_common::secp256k1::{PublicKey, Secp256k1, SecretKey};

        let secret = SecretKey::from_slice(&[1; 32]).unwrap();
        let public = PublicKey::from_secret_key(&Secp256k1::new(), &secret);
        let body: OpenChannelBody = serde_json::from_value(json::json!({
            "nodePubkey": base64::engine::general_purpose::STANDARD.encode(public.serialize()),
            "localFundingAmount": 100_000
        }))
        .unwrap();
        assert_eq!(open_channel_node_id(&body).unwrap(), public.to_string());
    }

    #[test]
    fn preserves_channel_push_amount_and_force_close_query() {
        let body: OpenChannelBody = serde_json::from_value(json::json!({
            "nodePubkeyString": "02",
            "localFundingAmount": "100000",
            "pushSat": "25000"
        }))
        .unwrap();
        assert_eq!(open_channel_push_msat(&body).unwrap(), Some(25_000_000));

        let excessive: OpenChannelBody = serde_json::from_value(json::json!({
            "nodePubkeyString": "02",
            "localFundingAmount": 100000,
            "pushSat": 100001
        }))
        .unwrap();
        assert!(open_channel_push_msat(&excessive).is_err());

        let query = web::Query::<CloseChannelQuery>::from_query("force=true").unwrap();
        assert!(query.force);
    }

    #[test]
    fn derives_value_and_fee_from_ldk_route_hops() {
        let result = json::json!({
            "path": [
                { "hop_fee_msat": 1250 },
                { "hop_fee_msat": 100_000 }
            ]
        });
        assert_eq!(route_value_and_fee(&result, 0), (100_000, 1250));
    }
}
