//! Actix HTTPS server for the LND-compatible REST API.
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use actix_web::{web, App, HttpServer};
use lampo_common::error;
use lampo_common::handler::Handler;
use lampod::LampoDaemon;
use tokio::sync::RwLock;

use crate::auth::MacaroonBakery;
use crate::auth::RequireMacaroon;
use crate::routes::{self, AppState, InvoiceIndex};
use crate::tls::TlsMaterial;

#[derive(Clone, Debug)]
pub struct LndRestConfig {
    pub listen_host: String,
    pub listen_port: u16,
    pub tls_dir: PathBuf,
    pub macaroon_dir: PathBuf,
}

impl LndRestConfig {
    pub fn socket_addr(&self) -> error::Result<SocketAddr> {
        let raw = format!("{}:{}", self.listen_host, self.listen_port);
        raw.parse()
            .map_err(|e| error::anyhow!("invalid lnd rest listen address `{raw}`: {e}"))
    }
}

/// Spawn the LND REST listener on a dedicated Actix system thread.
///
/// Actix's `HttpServer::run` future is `!Send`, so it cannot live on the
/// multi-thread tokio runtime used by `lampod-cli`.
pub fn spawn(lampod: Arc<LampoDaemon>, conf: LndRestConfig) -> error::Result<()> {
    let admin_path = conf.macaroon_dir.join("admin.macaroon");
    let addr = conf.socket_addr()?;
    thread::Builder::new()
        .name("lampo-lnd-rest".into())
        .spawn(move || {
            let system = actix_web::rt::System::new();
            if let Err(err) = system.block_on(run(lampod, conf)) {
                log::error!(target: "lampo-lnd", "LND REST server failed: {err}");
            }
        })
        .map_err(|e| error::anyhow!("failed to spawn LND REST thread: {e}"))?;

    // Wait until bakery material exists and the listener accepts TCP.
    for _ in 0..100 {
        if admin_path.exists() {
            if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(50)).is_ok() {
                return Ok(());
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    error::bail!(
        "timed out waiting for LND REST listener on {} (macaroon {})",
        addr,
        admin_path.display()
    )
}

/// Bind and run the LND REST listener. Returns once the server stops.
pub async fn run(lampod: Arc<LampoDaemon>, conf: LndRestConfig) -> error::Result<()> {
    // rustls 0.23 requires an explicit process-level CryptoProvider when
    // multiple backends could be linked. Prefer ring for portability.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let addr = conf.socket_addr()?;
    let hosts = vec![
        conf.listen_host.clone(),
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ];
    let tls = TlsMaterial::load_or_create(&conf.tls_dir, &hosts)
        .map_err(|e| error::anyhow!("tls init failed: {e}"))?;
    let rustls_config = tls
        .server_config()
        .map_err(|e| error::anyhow!("tls config failed: {e}"))?;
    let bakery = Arc::new(
        MacaroonBakery::load_or_create(&conf.macaroon_dir)
            .map_err(|e| error::anyhow!("macaroon bakery init failed: {e}"))?,
    );

    log::info!(
        target: "lampo-lnd",
        "LND REST listening on https://{} (tls={}, macaroons={})",
        addr,
        tls.cert_path.display(),
        bakery.macaroon_dir().display()
    );

    let state = web::Data::new(AppState {
        lampod: lampod.clone(),
        bakery,
        invoices: Arc::new(RwLock::new(InvoiceIndex::default())),
    });

    // Keep Zeus invoice polling accurate by marking in-memory invoices settled
    // when LDK claims an incoming payment.
    {
        let invoices = state.invoices.clone();
        let mut events = lampod.handler().events();
        actix_web::rt::spawn(async move {
            use lampo_common::event::Event;
            use lampo_common::ldk;
            while let Some(event) = events.recv().await {
                let Event::RawLDK(ldk::events::Event::PaymentClaimed {
                    payment_hash,
                    amount_msat,
                    purpose,
                    ..
                }) = event
                else {
                    continue;
                };
                let preimage = match purpose {
                    ldk::events::PaymentPurpose::Bolt11InvoicePayment {
                        payment_preimage, ..
                    }
                    | ldk::events::PaymentPurpose::Bolt12OfferPayment {
                        payment_preimage, ..
                    }
                    | ldk::events::PaymentPurpose::Bolt12RefundPayment {
                        payment_preimage, ..
                    } => payment_preimage,
                    ldk::events::PaymentPurpose::SpontaneousPayment(preimage) => Some(preimage),
                };
                let Some(preimage) = preimage else {
                    continue;
                };
                let key = hex::encode(payment_hash.0);
                let mut guard = invoices.write().await;
                if guard.mark_settled(
                    &key,
                    bytes::Bytes::from(preimage.0.to_vec()),
                    amount_msat as i64,
                ) {
                    log::debug!(
                        target: "lampo-lnd",
                        "marked invoice {key} settled for {amount_msat} msat"
                    );
                }
            }
        });
    }

    HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .app_data(web::JsonConfig::default().limit(1024 * 1024))
            .wrap(RequireMacaroon)
            .configure(routes::configure)
    })
    .bind_rustls_0_23(addr, rustls_config)
    .map_err(|e| error::anyhow!("failed to bind LND REST on {addr}: {e}"))?
    .run()
    .await
    .map_err(|e| error::anyhow!("LND REST server error: {e}"))?;

    Ok(())
}
