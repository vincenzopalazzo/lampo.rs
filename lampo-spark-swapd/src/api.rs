//! The daemon's own tiny HTTP API: request a Direction A quote,
//! publish a Direction B offer, list swaps. JSON in, JSON out,
//! mirroring the lampo-httpd style of one POST per method.
use std::sync::Arc;

use actix_web::{web, App, HttpResponse, HttpServer};
use serde::Deserialize;

use lampo_common::error;

use crate::engine::Engine;

#[derive(Deserialize)]
struct SwapOutRequest {
    /// The BOLT12 offer to pay from Spark.
    offer: String,
    /// Required when the offer has no amount.
    amount_msat: Option<u64>,
}

#[derive(Deserialize)]
struct SwapInRequest {
    /// Where the incoming funds land as a Spark HTLC.
    spark_address: String,
    /// How much the caller wants to receive on Spark, in sats. They pay
    /// this plus the fee on lightning.
    payout_sat: u64,
    /// Hash of a preimage *the caller* generated. They keep the
    /// preimage: it is what makes the swap atomic, and what they will
    /// use to claim the Spark HTLC.
    payment_hash: String,
}

async fn swap_out(engine: web::Data<Arc<Engine>>, body: web::Json<SwapOutRequest>) -> HttpResponse {
    match engine
        .quote_spark_to_ln(&body.offer, body.amount_msat)
        .await
    {
        Ok(quote) => HttpResponse::Ok().json(quote),
        Err(err) => error_response(err),
    }
}

async fn swap_in(engine: web::Data<Arc<Engine>>, body: web::Json<SwapInRequest>) -> HttpResponse {
    match engine
        .create_hold_swap(&body.spark_address, body.payout_sat, &body.payment_hash)
        .await
    {
        Ok(invoice) => HttpResponse::Ok().json(serde_json::json!({ "invoice": invoice })),
        Err(err) => error_response(err),
    }
}

async fn swaps(engine: web::Data<Arc<Engine>>) -> HttpResponse {
    // A sanitized view, never the raw record: the store keeps secrets
    // (the Direction A preimage) that must not leave the daemon.
    let listed: Vec<_> = engine
        .list()
        .into_iter()
        .map(|swap| {
            serde_json::json!({
                "payment_hash": swap.payment_hash,
                "direction": swap.direction,
                "state": swap.state,
                "amount_msat": swap.amount_msat,
                "spark_amount_sat": swap.spark_amount_sat,
                "created_at": swap.created_at,
                "updated_at": swap.updated_at,
            })
        })
        .collect();
    HttpResponse::Ok().json(listed)
}

fn error_response(err: error::Error) -> HttpResponse {
    HttpResponse::BadRequest().json(serde_json::json!({ "error": format!("{err}") }))
}

pub async fn run(engine: Arc<Engine>, addr: String) -> error::Result<()> {
    let server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(engine.clone()))
            .route("/v1/swap/out", web::post().to(swap_out))
            .route("/v1/swap/in", web::post().to(swap_in))
            .route("/v1/swaps", web::get().to(swaps))
    })
    .bind(&addr)
    .map_err(|err| error::anyhow!("cannot bind the swap api on `{addr}`: {err}"))?;
    log::info!(target: "swapd", "swap api listening on `{addr}`");
    server
        .run()
        .await
        .map_err(|err| error::anyhow!("swap api stopped: {err}"))
}
