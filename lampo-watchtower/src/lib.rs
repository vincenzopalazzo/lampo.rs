//! Watchtower client for lampo, speaking the Eye of Satoshi (TEOS)
//! protocol (<https://github.com/talaia-labs/rust-teos>).
//!
//! The [`WatchtowerPersister`] sits on lampo's monitor persistence path
//! and captures a signed justice transaction for every revoked
//! counterparty commitment. A background task delivers them to the
//! configured tower as encrypted appointments, retrying while the
//! tower is unreachable; state is file backed, so nothing is lost
//! across restarts.

pub mod client;
pub mod outbox;
pub mod persister;
pub mod teos;

pub mod error {
    pub use anyhow::*;
}

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bitcoin::secp256k1::{PublicKey, SecretKey};
use bitcoin::ScriptBuf;
use lightning::chain::chaininterface::FeeEstimator;
use tokio::sync::Notify;

use crate::client::{TowerClient, TowerError};
use crate::outbox::Outbox;
use crate::persister::WatchtowerCtx;
pub use crate::persister::WatchtowerPersister;

/// How long the delivery task waits between retries when the tower is
/// unreachable.
const RETRY_DELAY: Duration = Duration::from_secs(30);
/// Fallback poll interval of the delivery task.
const POLL_DELAY: Duration = Duration::from_secs(60);

/// Watchtower configuration, from lampo's conf file.
#[derive(Clone, Debug)]
pub struct WatchtowerConfig {
    /// Base URL of the tower's public API, e.g. `http://host:port`.
    pub tower_url: String,
    /// The tower id (public key) appointments receipts are verified
    /// against.
    pub tower_id: PublicKey,
    /// Lampo's network datadir; watchtower state lives in a
    /// `watchtower/` subdirectory.
    pub datadir: PathBuf,
}

/// Enables watchtower capture on the persister and spawns the delivery
/// task. Must run inside a tokio runtime.
pub fn start(
    persister: &WatchtowerPersister,
    config: WatchtowerConfig,
    destination_script: ScriptBuf,
    fee_estimator: Arc<dyn FeeEstimator + Send + Sync>,
) -> error::Result<()> {
    let root = config.datadir.join("watchtower");
    let outbox = Outbox::new(root.clone())?;
    let user_sk = load_or_create_user_sk(&root)?;
    let notifier = Arc::new(Notify::new());

    persister.enable(WatchtowerCtx {
        outbox: Outbox::new(root)?,
        destination_script,
        fee_estimator,
        notifier: notifier.clone(),
    });

    let client = TowerClient::new(config.tower_url.clone(), config.tower_id, user_sk);
    log::info!(
        target: "lampo-watchtower",
        "watchtower client enabled: tower `{}` at `{}`, user id `{}`",
        config.tower_id, config.tower_url, client.user_id()
    );
    tokio::spawn(delivery_loop(client, outbox, notifier));
    Ok(())
}

/// Loads the user secret key from `<root>/user_sk`, creating a fresh
/// one on first run.
fn load_or_create_user_sk(root: &PathBuf) -> error::Result<SecretKey> {
    let path = root.join("user_sk");
    if path.exists() {
        let hex_sk = std::fs::read_to_string(&path)?;
        return SecretKey::from_str(hex_sk.trim()).map_err(|err| error::anyhow!("{err}"));
    }
    let sk = loop {
        let mut bytes = [0u8; 32];
        rand::Rng::fill(&mut rand::thread_rng(), &mut bytes[..]);
        if let Ok(sk) = SecretKey::from_slice(&bytes) {
            break sk;
        }
    };
    let secret = hex::encode(sk.secret_bytes());
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(secret.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(secret.as_bytes())?;
        file.sync_all()?;
    }
    Ok(sk)
}

/// Drains the outbox to the tower forever: registers, delivers, backs
/// off while unreachable, and re-registers when the subscription runs
/// out.
async fn delivery_loop(client: TowerClient, outbox: Outbox, notifier: Arc<Notify>) {
    let mut registered = false;
    loop {
        if !registered {
            match client.register().await {
                Ok(receipt) => {
                    log::info!(
                        target: "lampo-watchtower",
                        "registered with the tower: {} slots, subscription expires at block {}",
                        receipt.available_slots, receipt.subscription_expiry
                    );
                    registered = true;
                }
                Err(err) => {
                    log::warn!(target: "lampo-watchtower", "tower registration failed: {err}");
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
            }
        }

        let entries = match outbox.list_signed() {
            Ok(entries) => entries,
            Err(err) => {
                log::error!(target: "lampo-watchtower", "cannot read the outbox: {err}");
                tokio::time::sleep(RETRY_DELAY).await;
                continue;
            }
        };

        let mut backoff = false;
        for signed in entries {
            match client.add_appointment(&signed).await {
                Ok(response) => {
                    log::debug!(
                        target: "lampo-watchtower",
                        "appointment for dispute {} accepted, {} slots left",
                        signed.dispute_txid, response.available_slots
                    );
                    if let Err(err) = outbox.remove_signed(&signed.dispute_txid) {
                        log::error!(target: "lampo-watchtower", "cannot clean the outbox: {err}");
                    }
                }
                Err(TowerError::Api(api)) => match api.error_code {
                    teos::errors::INVALID_SIGNATURE_OR_SUBSCRIPTION_ERROR
                    | teos::errors::REGISTRATION_RESOURCE_EXHAUSTED => {
                        log::warn!(
                            target: "lampo-watchtower",
                            "subscription problem ({}), re-registering", api.error
                        );
                        registered = false;
                        break;
                    }
                    teos::errors::APPOINTMENT_ALREADY_TRIGGERED => {
                        // The breach already hit the chain: our own
                        // monitor reacts to it, the tower cannot help
                        // with this state anymore.
                        log::warn!(
                            target: "lampo-watchtower",
                            "appointment for dispute {} already triggered", signed.dispute_txid
                        );
                        let _ = outbox.remove_signed(&signed.dispute_txid);
                    }
                    _ => {
                        log::error!(
                            target: "lampo-watchtower",
                            "tower rejected appointment for dispute {}: {} (code {})",
                            signed.dispute_txid, api.error, api.error_code
                        );
                        // Keep unknown API failures queued. New tower versions may add
                        // transient error codes, and dropping a justice transaction here
                        // would permanently remove breach protection.
                    }
                },
                Err(TowerError::Signature(err)) => {
                    // Keep the entry: a receipt with a bad signature is
                    // tower misbehavior, retrying later is the best we
                    // can do with a single tower.
                    log::error!(target: "lampo-watchtower", "{err}");
                    backoff = true;
                    break;
                }
                Err(TowerError::Connection(err)) => {
                    log::warn!(target: "lampo-watchtower", "tower unreachable: {err}");
                    backoff = true;
                    break;
                }
            }
        }

        if backoff {
            tokio::time::sleep(RETRY_DELAY).await;
        } else {
            tokio::select! {
                _ = notifier.notified() => {}
                _ = tokio::time::sleep(POLL_DELAY) => {}
            }
        }
    }
}
