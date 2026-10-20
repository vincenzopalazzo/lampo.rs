//! Persistence wiring for lampod.
//!
//! The daemon holds its store as [`LampoPersistenceBackend`], so which backend
//! it is gets decided here and nowhere else.
//!
//! N.B: the persistence has not been hardened for production use yet. Run a
//! node with funds you can afford to lose.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::ldk::io::{self, ErrorKind};
use lampo_common::ldk::util::persist::{
    KVStoreSync, CHANNEL_MANAGER_PERSISTENCE_KEY, CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
    CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE, CHANNEL_MONITOR_PERSISTENCE_PRIMARY_NAMESPACE,
    CHANNEL_MONITOR_PERSISTENCE_SECONDARY_NAMESPACE,
};
use lampo_common::persist::{
    FsPersistence, LampoPersistenceBackend, PaymentFilter, PaymentRecord, PaymentStore,
    PersistenceKind, STORAGE_SELECTION_FILE,
};
use lampo_vss::{canonical_vss_url, VssStore};

/// Build the persistence backend described by `conf`.
///
/// Unset, or `fs`, keeps LDK's filesystem store under the node directory.
/// `vss` keeps all Lampo and LDK state in the configured VSS store.
pub fn persistence_for(
    conf: &LampoConf,
    node_id: &str,
    storage_key: [u8; 32],
) -> error::Result<Arc<dyn LampoPersistenceBackend>> {
    let kind = conf.storage.as_deref().unwrap_or("fs");
    let store_id = format!("lampo-{}-{node_id}", conf.network);
    let selection = storage_selection(conf, kind, &store_id)?;
    let record_selection = validate_storage_selection(conf, &selection)?;
    let persistence: Arc<dyn LampoPersistenceBackend> = match kind {
        "fs" => Arc::new(FsPersistence::new(conf.path().into())),
        "vss" => {
            let url = conf.storage_url.as_deref().ok_or_else(|| {
                error::anyhow!("storage=vss needs storage-url set to a VSS server URL")
            })?;
            let safe_url = canonical_vss_url(url)?;
            log::info!(target: "lampod", "persisting to the VSS server at {safe_url}");
            let store = VssStore::new(
                url,
                &store_id,
                storage_key,
                conf.storage_auth_token.clone(),
                vss_global_version_path(conf),
            )?;
            Arc::new(FundCriticalMarkerStore::new(
                store,
                vss_initialized_path(conf),
            ))
        }
        other => error::bail!("unknown storage backend `{other}`, expected fs or vss"),
    };
    if record_selection {
        record_storage_selection(conf, &selection)?;
    }
    if kind == "vss" {
        refuse_lost_vss_state(conf, persistence.as_ref())?;
    }
    Ok(persistence)
}

const VSS_INITIALIZED_FILE: &str = ".lampo-vss-initialized";
const VSS_GLOBAL_VERSION_FILE: &str = ".lampo-vss-global-version";

struct FundCriticalMarkerStore<T> {
    inner: T,
    marker: PathBuf,
    marker_lock: Mutex<()>,
}

impl<T> FundCriticalMarkerStore<T> {
    fn new(inner: T, marker: PathBuf) -> Self {
        Self {
            inner,
            marker,
            marker_lock: Mutex::new(()),
        }
    }

    fn record_marker(&self) -> error::Result<()> {
        let _guard = self
            .marker_lock
            .lock()
            .map_err(|err| error::anyhow!("VSS initialization marker lock poisoned: {err}"))?;
        if !self.marker.exists() {
            record_vss_initialized_at(&self.marker)?;
        }
        Ok(())
    }
}

impl<T: LampoPersistenceBackend> KVStoreSync for FundCriticalMarkerStore<T> {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, io::Error> {
        self.inner.read(primary_namespace, secondary_namespace, key)
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), io::Error> {
        self.inner
            .write(primary_namespace, secondary_namespace, key, buf)?;
        if is_fund_critical(primary_namespace, secondary_namespace, key) {
            self.record_marker()
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;
        }
        Ok(())
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        lazy: bool,
    ) -> Result<(), io::Error> {
        self.inner
            .remove(primary_namespace, secondary_namespace, key, lazy)
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, io::Error> {
        self.inner.list(primary_namespace, secondary_namespace)
    }
}

impl<T: LampoPersistenceBackend> PaymentStore for FundCriticalMarkerStore<T> {
    fn upsert_payment(&self, payment: &PaymentRecord) -> error::Result<()> {
        self.inner.upsert_payment(payment)
    }

    fn get_payment(&self, id: &str) -> error::Result<Option<PaymentRecord>> {
        self.inner.get_payment(id)
    }

    fn list_payments(&self, filter: &PaymentFilter) -> error::Result<Vec<PaymentRecord>> {
        self.inner.list_payments(filter)
    }
}

impl<T: LampoPersistenceBackend> LampoPersistenceBackend for FundCriticalMarkerStore<T> {
    fn kind(&self) -> PersistenceKind {
        self.inner.kind()
    }

    fn list_all_keys(&self) -> error::Result<Vec<(String, String, String)>> {
        self.inner.list_all_keys()
    }
}

fn is_fund_critical(primary_namespace: &str, secondary_namespace: &str, key: &str) -> bool {
    (primary_namespace == CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE
        && secondary_namespace == CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE
        && key == CHANNEL_MANAGER_PERSISTENCE_KEY)
        || (primary_namespace == CHANNEL_MONITOR_PERSISTENCE_PRIMARY_NAMESPACE
            && secondary_namespace == CHANNEL_MONITOR_PERSISTENCE_SECONDARY_NAMESPACE)
}

/// A funded VSS node keeps a local marker after fund-critical state exists.
/// If that marker is present and the remote channel manager is gone, the
/// store was reset. Starting fresh under the same node id would orphan funds.
fn refuse_lost_vss_state(
    conf: &LampoConf,
    persistence: &dyn LampoPersistenceBackend,
) -> error::Result<()> {
    let marker = Path::new(&conf.path()).join(VSS_INITIALIZED_FILE);
    let initialized = match fs::read_to_string(&marker) {
        Ok(contents) => !contents.trim().is_empty(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => {
            return Err(error::anyhow!(
                "reading VSS initialization marker `{}`: {err}",
                marker.display()
            ))
        }
    };
    let has_manager = match persistence.read(
        CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
        CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
        CHANNEL_MANAGER_PERSISTENCE_KEY,
    ) {
        Ok(_) => true,
        Err(err) if err.kind() == ErrorKind::NotFound => false,
        Err(err) => return Err(error::anyhow!("reading VSS channel manager: {err}")),
    };
    let monitors = persistence
        .list(
            CHANNEL_MONITOR_PERSISTENCE_PRIMARY_NAMESPACE,
            CHANNEL_MONITOR_PERSISTENCE_SECONDARY_NAMESPACE,
        )
        .map_err(|err| error::anyhow!("listing VSS channel monitors: {err}"))?;
    if !monitors.is_empty() && !has_manager {
        error::bail!(
            "VSS contains channel monitors but no channel manager; \
             refusing to start because the remote state is incomplete"
        );
    }
    if initialized && !has_manager {
        error::bail!(
            "VSS was initialized for this node but the channel manager is missing;              refusing to start a fresh node under the same identity"
        );
    }
    if has_manager && !initialized {
        record_vss_initialized(conf)?;
    }
    Ok(())
}

fn record_vss_initialized(conf: &LampoConf) -> error::Result<()> {
    record_vss_initialized_at(&vss_initialized_path(conf))
}

fn vss_initialized_path(conf: &LampoConf) -> PathBuf {
    Path::new(&conf.path()).join(VSS_INITIALIZED_FILE)
}

fn vss_global_version_path(conf: &LampoConf) -> PathBuf {
    Path::new(&conf.path()).join(VSS_GLOBAL_VERSION_FILE)
}

fn record_vss_initialized_at(marker: &Path) -> error::Result<()> {
    let parent = marker
        .parent()
        .ok_or_else(|| error::anyhow!("VSS initialization marker has no parent directory"))?;
    fs::create_dir_all(parent)?;
    let temporary = marker.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(b"initialized\n")?;
    file.sync_all()?;
    fs::rename(&temporary, &marker)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Keep backend choice outside the backend itself so selecting an empty store
/// cannot make an existing node silently start from fresh state.
fn validate_storage_selection(conf: &LampoConf, selection: &str) -> error::Result<bool> {
    let marker = storage_selection_path(conf);
    let kind = selection
        .split_once(':')
        .map_or(selection, |(kind, _)| kind);
    match fs::read_to_string(&marker) {
        Ok(selected) if selected.trim() == selection => Ok(false),
        Ok(selected) => error::bail!(
            "storage backend is recorded as `{}` but configuration selects `{selection}`; \
             migrate the node state before changing storage",
            selected.trim()
        ),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if kind != "fs" && legacy_filesystem_has_channel_state(conf)? {
                error::bail!(
                    "existing filesystem channel state found; migrate it before selecting \
                     storage backend `{kind}`"
                );
            }
            Ok(true)
        }
        Err(err) => Err(error::anyhow!(
            "reading storage backend marker `{}`: {err}",
            marker.display()
        )),
    }
}

fn storage_selection(conf: &LampoConf, kind: &str, store_id: &str) -> error::Result<String> {
    match kind {
        "fs" => Ok("fs".to_owned()),
        "vss" => {
            let url = conf.storage_url.as_deref().ok_or_else(|| {
                error::anyhow!("storage=vss needs storage-url set to a VSS server URL")
            })?;
            Ok(format!("vss:{}:{store_id}", canonical_vss_url(url)?))
        }
        other => error::bail!("unknown storage backend `{other}`, expected fs or vss"),
    }
}

fn legacy_filesystem_has_channel_state(conf: &LampoConf) -> error::Result<bool> {
    let store = FsPersistence::new(conf.path().into());
    match KVStoreSync::read(
        &store,
        CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
        CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
        CHANNEL_MANAGER_PERSISTENCE_KEY,
    ) {
        Ok(_) => return Ok(true),
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(error::anyhow!("checking filesystem channel manager: {err}")),
    }

    let monitors = KVStoreSync::list(
        &store,
        CHANNEL_MONITOR_PERSISTENCE_PRIMARY_NAMESPACE,
        CHANNEL_MONITOR_PERSISTENCE_SECONDARY_NAMESPACE,
    )
    .map_err(|err| error::anyhow!("checking filesystem channel monitors: {err}"))?;
    Ok(!monitors.is_empty())
}

fn record_storage_selection(conf: &LampoConf, selection: &str) -> error::Result<()> {
    let marker = storage_selection_path(conf);
    let parent = marker
        .parent()
        .ok_or_else(|| error::anyhow!("storage backend marker has no parent directory"))?;
    fs::create_dir_all(parent)?;
    let temporary = marker.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(format!("{selection}\n").as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, &marker)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn storage_selection_path(conf: &LampoConf) -> PathBuf {
    Path::new(&conf.path()).join(STORAGE_SELECTION_FILE)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "lampo-storage-selection-{name}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn conf(&self) -> LampoConf {
            LampoConf {
                root_path: self.0.to_string_lossy().into_owned(),
                ..LampoConf::default()
            }
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn recorded_backend_switch_is_rejected() {
        let dir = ScratchDir::new("recorded");
        let conf = dir.conf();
        record_storage_selection(&conf, "fs").unwrap();

        assert!(!validate_storage_selection(&conf, "fs").unwrap());
        assert!(validate_storage_selection(&conf, "vss:https://example.test:node").is_err());
    }

    #[test]
    fn unknown_storage_backend_is_rejected() {
        let conf = LampoConf::default();
        assert!(storage_selection(&conf, "sqlite", "node").is_err());
        assert!(storage_selection(&conf, "postgres", "node").is_err());
        assert_eq!(storage_selection(&conf, "fs", "node").unwrap(), "fs");
    }

    #[test]
    fn vss_requires_a_url_and_records_its_destination() {
        let mut conf = LampoConf::default();
        assert!(storage_selection(&conf, "vss", "node").is_err());

        conf.storage_url = Some("https://vss.example.test".to_owned());
        assert_eq!(
            storage_selection(&conf, "vss", "node").unwrap(),
            "vss:https://vss.example.test:node"
        );
    }

    #[test]
    fn vss_selection_excludes_rotating_url_credentials() {
        let mut conf = LampoConf {
            storage_url: Some(
                "https://user:password@VSS.EXAMPLE.test:443/api/?token=first".to_owned(),
            ),
            ..LampoConf::default()
        };
        let first = storage_selection(&conf, "vss", "node").unwrap();
        conf.storage_url = Some("https://vss.example.test/api?token=second".to_owned());
        let second = storage_selection(&conf, "vss", "node").unwrap();

        assert_eq!(first, "vss:https://vss.example.test/api:node");
        assert_eq!(first, second);
        assert!(!first.contains("password"));
        assert!(!first.contains("token"));
    }

    #[test]
    fn existing_filesystem_state_blocks_vss_selection() {
        let dir = ScratchDir::new("legacy-filesystem");
        let conf = dir.conf();
        let store = FsPersistence::new(conf.path().into());
        KVStoreSync::write(
            &store,
            CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
            CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
            CHANNEL_MANAGER_PERSISTENCE_KEY,
            b"manager".to_vec(),
        )
        .unwrap();

        assert!(validate_storage_selection(&conf, "vss:https://vss.example.test:node").is_err());
        assert!(!storage_selection_path(&conf).exists());
    }

    #[test]
    fn empty_vss_does_not_create_initialized_marker() {
        let dir = ScratchDir::new("empty-vss");
        let conf = dir.conf();
        let store = FsPersistence::new(conf.path().into());

        refuse_lost_vss_state(&conf, &store).unwrap();

        assert!(!Path::new(&conf.path()).join(VSS_INITIALIZED_FILE).exists());
    }

    #[test]
    fn first_fund_critical_write_creates_initialized_marker() {
        let dir = ScratchDir::new("first-critical-write");
        let conf = dir.conf();
        let marker = vss_initialized_path(&conf);
        let store =
            FundCriticalMarkerStore::new(FsPersistence::new(conf.path().into()), marker.clone());
        assert!(!marker.exists());

        KVStoreSync::write(
            &store,
            CHANNEL_MONITOR_PERSISTENCE_PRIMARY_NAMESPACE,
            CHANNEL_MONITOR_PERSISTENCE_SECONDARY_NAMESPACE,
            "funding-outpoint",
            b"monitor".to_vec(),
        )
        .unwrap();

        assert!(marker.exists());
    }

    #[test]
    fn monitors_without_manager_are_rejected_without_local_marker() {
        let dir = ScratchDir::new("orphan-monitor");
        let conf = dir.conf();
        let store = FsPersistence::new(conf.path().into());
        KVStoreSync::write(
            &store,
            CHANNEL_MONITOR_PERSISTENCE_PRIMARY_NAMESPACE,
            CHANNEL_MONITOR_PERSISTENCE_SECONDARY_NAMESPACE,
            "funding-outpoint",
            b"monitor".to_vec(),
        )
        .unwrap();

        let err = refuse_lost_vss_state(&conf, &store)
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("channel monitors but no channel manager"),
            "unexpected error: {err}"
        );
        assert!(!Path::new(&conf.path()).join(VSS_INITIALIZED_FILE).exists());
    }

    #[test]
    fn manager_state_creates_initialized_marker() {
        let dir = ScratchDir::new("manager-vss");
        let conf = dir.conf();
        let store = FsPersistence::new(conf.path().into());
        KVStoreSync::write(
            &store,
            CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
            CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
            CHANNEL_MANAGER_PERSISTENCE_KEY,
            b"manager".to_vec(),
        )
        .unwrap();

        refuse_lost_vss_state(&conf, &store).unwrap();

        assert!(Path::new(&conf.path()).join(VSS_INITIALIZED_FILE).exists());
    }
}
