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
use std::sync::Arc;

use lampo_common::conf::LampoConf;
use lampo_common::error;
use lampo_common::ldk::io::ErrorKind;
use lampo_common::ldk::util::persist::{
    KVStoreSync, CHANNEL_MANAGER_PERSISTENCE_KEY, CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
    CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE, CHANNEL_MONITOR_PERSISTENCE_PRIMARY_NAMESPACE,
    CHANNEL_MONITOR_PERSISTENCE_SECONDARY_NAMESPACE,
};
use lampo_common::persist::{FsPersistence, LampoPersistenceBackend, STORAGE_SELECTION_FILE};
use lampo_vss::VssStore;

/// Build the persistence backend described by `conf`.
///
/// Unset, or `fs`, keeps LDK's filesystem store under the node directory.
/// `vss` keeps all Lampo and LDK state in the configured VSS store.
pub fn persistence_for(
    conf: &LampoConf,
    node_id: &str,
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
            log::info!(target: "lampod", "persisting to the VSS server at {url}");
            Arc::new(VssStore::new(url, &store_id)?)
        }
        other => error::bail!("unknown storage backend `{other}`, expected fs or vss"),
    };
    if record_selection {
        record_storage_selection(conf, &selection)?;
    }
    Ok(persistence)
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
            Ok(format!("vss:{url}:{store_id}"))
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
}
