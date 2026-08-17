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
use lampo_postgres::{destination_id as postgres_destination_id, PostgresStore};
use lampo_sqlite::SqliteStore;

/// Build the persistence backend described by `conf`.
///
/// Unset, or `fs`, keeps LDK's filesystem store under the node directory. The
/// database backends need `storage-url`.
pub fn persistence_for(conf: &LampoConf) -> error::Result<Arc<dyn LampoPersistenceBackend>> {
    let kind = conf.storage.as_deref().unwrap_or("fs");
    let selection = storage_selection(conf, kind)?;
    let record_selection = validate_storage_selection(conf, &selection)?;
    let persistence: Arc<dyn LampoPersistenceBackend> = match kind {
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
    if record_selection {
        record_storage_selection(conf, &selection)?;
    }
    Ok(persistence)
}

/// Keep backend choice and destination outside the backend itself so selecting
/// an empty store cannot make an existing node silently start from fresh state.
fn validate_storage_selection(conf: &LampoConf, selection: &str) -> error::Result<bool> {
    let marker = storage_selection_path(conf);
    let kind = selection_kind(selection);
    match fs::read_to_string(&marker) {
        Ok(selected) if selected.trim() == selection => Ok(false),
        // Older markers recorded only the backend kind. Accept them when the
        // kind still matches, then rewrite with the destination fingerprint.
        Ok(selected) if selected.trim() == kind => Ok(true),
        Ok(selected) => error::bail!(
            "storage backend is recorded as `{}` but configuration selects `{selection}`; \
             migrate the node state before changing storage or storage-url",
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

fn selection_kind(selection: &str) -> &str {
    selection
        .split_once(':')
        .map(|(kind, _)| kind)
        .unwrap_or(selection)
}

fn storage_selection(conf: &LampoConf, kind: &str) -> error::Result<String> {
    match kind {
        "fs" => Ok("fs".to_owned()),
        "sqlite" => {
            let path = match conf.storage_url.as_deref() {
                Some(url) => url.to_owned(),
                None => format!("{}/lampo.db", conf.path()),
            };
            Ok(format!(
                "sqlite:{}",
                sqlite_destination_path(&path).display()
            ))
        }
        "postgres" => {
            let url = conf.storage_url.as_deref().ok_or_else(|| {
                error::anyhow!("storage=postgres needs storage-url set to a postgres:// URL")
            })?;
            Ok(format!("postgres:{}", postgres_destination_id(url)?))
        }
        other => error::bail!("unknown storage backend `{other}`, expected fs, sqlite or postgres"),
    }
}

/// Resolve a SQLite path the same way before and after the file exists.
///
/// `canonicalize` fails for a missing file, so a relative `storage-url` would
/// otherwise be recorded raw on first start and rewritten to an absolute path
/// on the next start, looking like a destination change. Canonicalizing the
/// parent when the file is absent keeps macOS `/var` → `/private/var` stable too.
fn sqlite_destination_path(path: &str) -> PathBuf {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    if let Ok(canonical) = fs::canonicalize(&absolute) {
        return canonical;
    }
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => match fs::canonicalize(parent) {
            Ok(parent) => parent.join(name),
            Err(_) => absolute,
        },
        _ => absolute,
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
        assert!(validate_storage_selection(&conf, "sqlite:/tmp/other.db").is_err());
    }

    #[test]
    fn legacy_filesystem_state_blocks_database_selection() {
        let dir = ScratchDir::new("legacy");
        let conf = dir.conf();
        let store = FsPersistence::new(conf.path().into());
        KVStoreSync::write(
            &store,
            CHANNEL_MANAGER_PERSISTENCE_PRIMARY_NAMESPACE,
            CHANNEL_MANAGER_PERSISTENCE_SECONDARY_NAMESPACE,
            CHANNEL_MANAGER_PERSISTENCE_KEY,
            b"existing-manager".to_vec(),
        )
        .unwrap();

        assert!(validate_storage_selection(&conf, "sqlite:/tmp/other.db").is_err());
        assert!(!storage_selection_path(&conf).exists());
    }

    #[test]
    fn recorded_sqlite_destination_switch_is_rejected() {
        let dir = ScratchDir::new("sqlite-destination");
        let mut conf = dir.conf();
        conf.storage = Some("sqlite".to_owned());
        conf.storage_url = Some(format!("{}/a.db", conf.path()));
        let first = storage_selection(&conf, "sqlite").unwrap();
        record_storage_selection(&conf, &first).unwrap();

        conf.storage_url = Some(format!("{}/b.db", conf.path()));
        let second = storage_selection(&conf, "sqlite").unwrap();
        assert!(validate_storage_selection(&conf, &second).is_err());
    }

    #[test]
    fn relative_sqlite_path_stays_stable_after_file_appears() {
        let dir = ScratchDir::new("sqlite-relative");
        let cwd = std::env::current_dir().unwrap();
        let absolute = dir.0.join("relative.db");
        let relative = absolute
            .strip_prefix(&cwd)
            .map(|rel| rel.display().to_string())
            .unwrap_or_else(|_| absolute.display().to_string());
        let before = sqlite_destination_path(&relative);
        fs::write(&absolute, b"").unwrap();
        let after = sqlite_destination_path(&relative);
        assert_eq!(
            before, after,
            "creating the database must not change the recorded destination"
        );
        assert!(
            before.is_absolute(),
            "relative storage-url must be recorded as an absolute path"
        );
    }
}
