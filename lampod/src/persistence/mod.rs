//! Persistence wiring for lampod.
//!
//! The daemon holds its store as [`LampoPersistenceBackend`], so which backend
//! it is gets decided here and nowhere else.
//!
//! N.B: the persistence has not been hardened for production use yet. Run a
//! node with funds you can afford to lose.
use std::sync::Arc;

use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::persist::{FsPersistence, LampoPersistenceBackend};
use lampo_postgres::PostgresStore;
use lampo_sqlite::SqliteStore;
use lampo_vss::{VssShadow, VssSink};

/// Build the persistence backend described by `conf`.
///
/// Unset, or `fs`, keeps LDK's filesystem store under the node directory. The
/// database backends need `storage-url`. Setting `vss-url` additionally keeps a
/// shadow copy of whatever was chosen, for recovery.
pub fn persistence_for(
    conf: &LampoConf,
    node_id: &str,
) -> error::Result<Arc<dyn LampoPersistenceBackend>> {
    let kind = conf.storage.as_deref().unwrap_or("fs");
    let primary: Arc<dyn LampoPersistenceBackend> = match kind {
        "fs" => Arc::new(FsPersistence::new(conf.path().into())),
        "sqlite" => {
            // Default to a file inside the node directory, so `storage=sqlite`
            // on its own is enough to get going.
            let path = match conf.storage_url.as_deref() {
                Some(url) => url.to_owned(),
                None => format!("{}/lampo.db", conf.path()),
            };
            log::info!(target: "lampod", "persisting to the sqlite database at {path}");
            Arc::new(SqliteStore::new(&path)?)
        }
        "postgres" => {
            let url = conf.storage_url.as_deref().ok_or_else(|| {
                error::anyhow!("storage=postgres needs storage-url set to a postgres:// URL")
            })?;
            log::info!(target: "lampod", "persisting to postgres");
            Arc::new(PostgresStore::new(url)?)
        }
        other => error::bail!("unknown storage backend `{other}`, expected fs, sqlite or postgres"),
    };

    let Some(vss_url) = conf.vss_url.as_deref() else {
        return Ok(primary);
    };
    // The store id has to keep nodes apart, not just networks: two nodes on
    // one server sharing an id would overwrite each other's copy, and neither
    // would be safe to restore.
    let store_id = format!("lampo-{}-{}", conf.network, node_id);
    log::info!(target: "lampod", "mirroring state to the VSS server at {vss_url}");
    Ok(VssShadow::wrap(
        primary,
        Arc::new(VssSink::new(vss_url, &store_id)?),
    )?)
}
