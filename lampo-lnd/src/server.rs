//! Actix HTTPS server for the LND-compatible REST API.
use std::path::PathBuf;
use std::sync::mpsc;
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
    pub tls_extra_sans: Vec<String>,
    pub tls_dir: PathBuf,
    pub macaroon_dir: PathBuf,
}

/// Spawn the LND REST listener on a dedicated Actix system thread.
///
/// Actix's `HttpServer::run` future is `!Send`, so it cannot live on the
/// multi-thread tokio runtime used by `lampod-cli`.
pub fn spawn(lampod: Arc<LampoDaemon>, conf: LndRestConfig) -> error::Result<()> {
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("lampo-lnd-rest".into())
        .spawn(move || {
            let system = actix_web::rt::System::new();
            if let Err(err) = system.block_on(run_with_ready(lampod, conf, Some(ready_tx))) {
                log::error!(target: "lampo-lnd", "LND REST server failed: {err}");
            }
        })
        .map_err(|e| error::anyhow!("failed to spawn LND REST thread: {e}"))?;

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(error::anyhow!(err)),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            error::bail!("timed out waiting for LND REST listener startup")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            error::bail!("LND REST server exited before reporting startup")
        }
    }
}

/// Bind and run the LND REST listener. Returns once the server stops.
pub async fn run(lampod: Arc<LampoDaemon>, conf: LndRestConfig) -> error::Result<()> {
    run_with_ready(lampod, conf, None).await
}

async fn run_with_ready(
    lampod: Arc<LampoDaemon>,
    conf: LndRestConfig,
    ready: Option<mpsc::SyncSender<Result<(), String>>>,
) -> error::Result<()> {
    let server = match build_server(lampod, conf) {
        Ok(server) => server,
        Err(err) => {
            if let Some(ready) = ready {
                let _ = ready.send(Err(err.to_string()));
            }
            return Err(err);
        }
    };
    if let Some(ready) = ready {
        let _ = ready.send(Ok(()));
    }
    server
        .await
        .map_err(|e| error::anyhow!("LND REST server error: {e}"))
}

fn build_server(
    lampod: Arc<LampoDaemon>,
    conf: LndRestConfig,
) -> error::Result<actix_web::dev::Server> {
    // rustls 0.23 requires an explicit process-level CryptoProvider when
    // multiple backends could be linked. Prefer ring for portability.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let addr = if conf.listen_host.contains(':') {
        format!("[{}]:{}", conf.listen_host, conf.listen_port)
    } else {
        format!("{}:{}", conf.listen_host, conf.listen_port)
    };
    let hosts = certificate_hosts(&conf);
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

    let listen_host = conf.listen_host.clone();
    let listen_port = conf.listen_port;
    let server = HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .app_data(web::JsonConfig::default().limit(1024 * 1024))
            .wrap(RequireMacaroon)
            .configure(routes::configure)
    })
    .bind_rustls_0_23((listen_host.as_str(), listen_port), rustls_config)
    .map_err(|e| error::anyhow!("failed to bind LND REST on {addr}: {e}"))?
    .run();
    Ok(server)
}

fn certificate_hosts(conf: &LndRestConfig) -> Vec<String> {
    let mut hosts = vec![
        conf.listen_host.clone(),
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
    ];
    for host in &conf.tls_extra_sans {
        if !host.is_empty() && !hosts.contains(host) {
            hosts.push(host.clone());
        }
    }
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_remote_tls_names_with_wildcard_listener() {
        let conf = LndRestConfig {
            listen_host: "0.0.0.0".into(),
            listen_port: 8080,
            tls_extra_sans: vec!["node.local".into(), "192.168.1.10".into()],
            tls_dir: PathBuf::new(),
            macaroon_dir: PathBuf::new(),
        };

        let hosts = certificate_hosts(&conf);
        assert!(hosts.contains(&"node.local".to_string()));
        assert!(hosts.contains(&"192.168.1.10".to_string()));
        assert!(hosts.contains(&"localhost".to_string()));
    }
}
