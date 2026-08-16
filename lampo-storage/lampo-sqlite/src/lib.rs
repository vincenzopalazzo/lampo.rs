//! SQLite persistence backend.
//!
//! The zero-setup database backend: no server, one file, and the same schema
//! the Postgres backend uses. Core Lightning defaults to SQLite for its wallet
//! for the same reason.
//!
//! Every call goes through one connection behind a mutex. SQLite serialises
//! writers anyway, and lampo's write path is small blobs, so a pool would buy
//! nothing.
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::Mutex;

use lampo_common::error;
use lampo_common::ldk::io;
use lampo_common::ldk::util::persist::KVStoreSync;
use lampo_common::persist::{
    LampoPersistenceBackend, PaymentDirection, PaymentFilter, PaymentRecord, PaymentStatus,
    PaymentStore, PersistenceKind,
};
use lampo_storage_common as sql;
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, OptionalExtension};

/// SQLite-backed store.
pub struct SqliteStore {
    conn: Mutex<Connection>,
    /// Process-lifetime exclusive lock; kept open so a second lampo process
    /// cannot share the same database file.
    _writer_lock: Option<File>,
}

impl SqliteStore {
    /// Open (creating if needed) the database at `path` and migrate it.
    pub fn new(path: &str) -> error::Result<Self> {
        let writer_lock = acquire_writer_lock(path)?;
        let conn = Connection::open(path)?;
        Self::from_connection(conn, Some(writer_lock))
    }

    /// Open an in-memory database, for tests.
    pub fn in_memory() -> error::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, None)
    }

    fn from_connection(conn: Connection, writer_lock: Option<File>) -> error::Result<Self> {
        // A channel monitor that is acknowledged and then lost can make the
        // node broadcast a stale commitment, so the write has to reach the
        // disk, not just the operating system's cache.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let store = Self {
            conn: Mutex::new(conn),
            _writer_lock: writer_lock,
        };
        store.migrate()?;
        Ok(store)
    }

    /// Apply any migration the database has not seen yet.
    fn migrate(&self) -> error::Result<()> {
        let mut conn = self.lock()?;
        conn.execute_batch(sql::version_table(sql::Dialect::Sqlite))?;
        let applied: i64 = conn
            .query_row(sql::read_version(), [], |row| row.get(0))
            .optional()?
            .unwrap_or(0);

        for migration in sql::migrations(sql::Dialect::Sqlite) {
            if migration.version <= applied {
                continue;
            }
            // Schema change and version bump land together or not at all.
            let tx = conn.transaction()?;
            tx.execute_batch(migration.sql)?;
            tx.execute(
                sql::write_version(sql::Dialect::Sqlite),
                [migration.version],
            )?;
            tx.commit()?;
            log::info!(target: "lampo-sqlite", "applied schema migration {}", migration.version);
        }
        Ok(())
    }

    fn lock(&self) -> error::Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|err| error::anyhow!("sqlite connection poisoned: {err}"))
    }

    /// Same as [`Self::lock`], for the `KVStoreSync` methods that owe an
    /// `io::Error`.
    fn lock_io(&self) -> Result<std::sync::MutexGuard<'_, Connection>, io::Error> {
        self.conn
            .lock()
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))
    }
}

/// SQL failures reach LDK as `io::Error`, which is all its store trait speaks.
fn io_err(err: rusqlite::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, err.to_string())
}

/// Exclusive process-lifetime lock beside the database file.
///
/// WAL serialises statements but not Lightning writers: two nodes on one file
/// can still interleave channel-manager updates. Fail the second opener.
fn acquire_writer_lock(path: &str) -> error::Result<File> {
    let resolved = resolve_db_path(path);
    let lock_path = resolved.with_extension("writerlock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // LOCK_EX | LOCK_NB — exclusive, non-blocking.
        let rc = unsafe { libc_flock(file.as_raw_fd(), 2 | 4) };
        if rc != 0 {
            error::bail!(
                "another lampo process already holds the sqlite writer lock at {}",
                lock_path.display()
            );
        }
    }
    #[cfg(not(unix))]
    {
        let _ = &file;
        log::warn!(
            target: "lampo-sqlite",
            "sqlite writer lock is best-effort on this platform; refuse running two nodes on {}",
            path
        );
    }
    Ok(file)
}

/// Resolve aliases (relative paths, symlinks) to one lock target per inode.
fn resolve_db_path(path: &str) -> std::path::PathBuf {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    if let Ok(canonical) = std::fs::canonicalize(&absolute) {
        return canonical;
    }
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => match std::fs::canonicalize(parent) {
            Ok(parent) => parent.join(name),
            Err(_) => absolute,
        },
        _ => absolute,
    }
}

#[cfg(unix)]
unsafe fn libc_flock(fd: std::os::unix::io::RawFd, op: i32) -> i32 {
    extern "C" {
        fn flock(fd: i32, op: i32) -> i32;
    }
    flock(fd, op)
}

impl KVStoreSync for SqliteStore {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, io::Error> {
        let conn = self.lock_io()?;
        let value: Option<Vec<u8>> = conn
            .query_row(
                &sql::kv_read(sql::Dialect::Sqlite),
                (primary_namespace, secondary_namespace, key),
                |row| row.get(0),
            )
            .optional()
            .map_err(io_err)?;
        // Callers such as the payer proof store tell "no record" from "store
        // broken" by this error kind alone.
        value.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no value for {primary_namespace}/{secondary_namespace}/{key}"),
            )
        })
    }

    fn write(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        buf: Vec<u8>,
    ) -> Result<(), io::Error> {
        let conn = self.lock_io()?;
        conn.execute(
            sql::kv_write(sql::Dialect::Sqlite),
            rusqlite::params![primary_namespace, secondary_namespace, key, buf],
        )
        .map_err(io_err)?;
        Ok(())
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        _lazy: bool,
    ) -> Result<(), io::Error> {
        let conn = self.lock_io()?;
        // `lazy` only permits deferring the removal; doing it now is always
        // allowed and keeps the ordering guarantee trivially.
        conn.execute(
            &sql::kv_remove(sql::Dialect::Sqlite),
            (primary_namespace, secondary_namespace, key),
        )
        .map_err(io_err)?;
        Ok(())
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, io::Error> {
        let conn = self.lock_io()?;
        let mut stmt = conn
            .prepare(&sql::kv_list(sql::Dialect::Sqlite))
            .map_err(io_err)?;
        let keys = stmt
            .query_map((primary_namespace, secondary_namespace), |row| {
                row.get::<_, String>(0)
            })
            .map_err(io_err)?
            .collect::<Result<Vec<String>, _>>()
            .map_err(io_err)?;
        Ok(keys)
    }
}

/// Amounts are unsigned in lampo and signed in SQL; no real amount is anywhere
/// near the boundary, so a value that does not fit is a bug worth reporting.
fn to_column(value: u64, what: &str) -> error::Result<i64> {
    value
        .try_into()
        .map_err(|_| error::anyhow!("{what} {value} does not fit a 64-bit column"))
}

/// Bind a rendered parameter to the SQLite value that carries it.
fn to_sqlite_value(param: sql::QueryParam) -> Value {
    match param {
        sql::QueryParam::Int(value) => Value::Integer(value),
        sql::QueryParam::Text(value) => Value::Text(value),
        sql::QueryParam::Bytes(value) => Value::Blob(value),
        // SQLite is untyped enough not to care which null this was.
        sql::QueryParam::NullInt | sql::QueryParam::NullText => Value::Null,
    }
}

fn row_to_payment(row: &rusqlite::Row<'_>) -> rusqlite::Result<error::Result<PaymentRecord>> {
    let direction: String = row.get(2)?;
    let status: String = row.get(5)?;
    let amount_msat: i64 = row.get(3)?;
    let fee_msat: Option<i64> = row.get(4)?;
    let created_at: i64 = row.get(6)?;
    let id: String = row.get(0)?;
    let payment_hash: String = row.get(1)?;
    let invoice: Option<String> = row.get(7)?;

    Ok((|| {
        Ok(PaymentRecord {
            id,
            payment_hash,
            direction: PaymentDirection::from_str(&direction)?,
            amount_msat: amount_msat as u64,
            fee_msat: fee_msat.map(|fee| fee as u64),
            status: PaymentStatus::from_str(&status)?,
            created_at: created_at as u64,
            invoice,
        })
    })())
}

impl PaymentStore for SqliteStore {
    fn upsert_payment(&self, payment: &PaymentRecord) -> error::Result<()> {
        let conn = self.lock()?;
        conn.execute(
            sql::payment_write(sql::Dialect::Sqlite),
            rusqlite::params![
                payment.id,
                payment.payment_hash,
                payment.direction.as_str(),
                to_column(payment.amount_msat, "amount_msat")?,
                payment
                    .fee_msat
                    .map(|fee| to_column(fee, "fee_msat"))
                    .transpose()?,
                payment.status.as_str(),
                to_column(payment.created_at, "created_at")?,
                payment.invoice,
            ],
        )?;
        Ok(())
    }

    fn get_payment(&self, id: &str) -> error::Result<Option<PaymentRecord>> {
        let conn = self.lock()?;
        let record = conn
            .query_row(&sql::payment_read(sql::Dialect::Sqlite), [id], |row| {
                row_to_payment(row)
            })
            .optional()?;
        record.transpose()
    }

    fn list_payments(&self, filter: &PaymentFilter) -> error::Result<Vec<PaymentRecord>> {
        let query = sql::list_payments(filter, sql::Dialect::Sqlite)?;
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&query.sql)?;
        let values = query.params.into_iter().map(to_sqlite_value);
        let rows = stmt
            .query_map(params_from_iter(values), |row| row_to_payment(row))?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().collect()
    }
}

impl LampoPersistenceBackend for SqliteStore {
    fn kind(&self) -> PersistenceKind {
        PersistenceKind::Sqlite
    }

    fn list_all_keys(&self) -> error::Result<Vec<(String, String, String)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(sql::kv_list_all())?;
        let keys = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(keys)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn store() -> Arc<dyn LampoPersistenceBackend> {
        Arc::new(SqliteStore::in_memory().unwrap())
    }

    fn payment(id: &str, created_at: u64, status: PaymentStatus) -> PaymentRecord {
        PaymentRecord {
            id: id.to_owned(),
            payment_hash: format!("hash-{id}"),
            direction: PaymentDirection::Outbound,
            amount_msat: 1_000,
            fee_msat: Some(7),
            status,
            created_at,
            invoice: Some("lnbc1...".to_owned()),
        }
    }

    #[test]
    fn reports_its_kind() {
        assert_eq!(store().kind(), PersistenceKind::Sqlite);
    }

    #[test]
    fn a_second_opener_is_rejected() {
        let path = std::env::temp_dir().join(format!(
            "lampo-sqlite-lock-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = path.to_str().unwrap();
        let first = SqliteStore::new(path).unwrap();
        let err = match SqliteStore::new(path) {
            Ok(_) => panic!("second opener must fail"),
            Err(err) => err.to_string(),
        };
        assert!(
            err.contains("sqlite writer lock"),
            "second opener must fail: {err}"
        );
        drop(first);
        // Lock released; reopening must succeed.
        let _second = SqliteStore::new(path).unwrap();
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(Path::new(path).with_extension("writerlock"));
    }

    #[test]
    fn kv_round_trips_through_the_trait_object() {
        let store = store();
        store.write("ns", "sub", "key", b"value".to_vec()).unwrap();

        assert_eq!(store.read("ns", "sub", "key").unwrap(), b"value");
        assert_eq!(store.list("ns", "sub").unwrap(), vec!["key".to_string()]);

        // A second write to the same key replaces rather than conflicting.
        store.write("ns", "sub", "key", b"newer".to_vec()).unwrap();
        assert_eq!(store.read("ns", "sub", "key").unwrap(), b"newer");

        store.remove("ns", "sub", "key", false).unwrap();
        assert!(store.list("ns", "sub").unwrap().is_empty());
    }

    /// `payer_proof::load` maps this exact error kind to "no proof stored".
    #[test]
    fn reading_a_missing_key_reports_not_found() {
        let err = store().read("ns", "", "absent").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn namespaces_do_not_leak_into_each_other() {
        let store = store();
        store.write("a", "", "key", b"one".to_vec()).unwrap();
        store.write("b", "", "key", b"two".to_vec()).unwrap();

        assert_eq!(store.read("a", "", "key").unwrap(), b"one");
        assert_eq!(store.read("b", "", "key").unwrap(), b"two");
        assert_eq!(store.list("a", "").unwrap().len(), 1);
    }

    #[test]
    fn payments_round_trip_and_upsert_replaces() {
        let store = store();
        store
            .upsert_payment(&payment("a", 100, PaymentStatus::Pending))
            .unwrap();
        let stored = store.get_payment("a").unwrap().unwrap();
        assert_eq!(stored, payment("a", 100, PaymentStatus::Pending));

        store
            .upsert_payment(&payment("a", 100, PaymentStatus::Succeeded))
            .unwrap();
        assert_eq!(
            store.get_payment("a").unwrap().unwrap().status,
            PaymentStatus::Succeeded
        );
        assert_eq!(
            store
                .list_payments(&PaymentFilter::default())
                .unwrap()
                .len(),
            1,
            "upsert must replace, not append"
        );
        assert!(store.get_payment("absent").unwrap().is_none());
    }

    /// The reason the payments table exists: ask the database for a window and
    /// let the index do the work.
    #[test]
    fn list_payments_filters_in_sql() {
        let store = store();
        for (id, at) in [("a", 100), ("b", 200), ("c", 300)] {
            store
                .upsert_payment(&payment(id, at, PaymentStatus::Succeeded))
                .unwrap();
        }
        store
            .upsert_payment(&payment("d", 250, PaymentStatus::Failed))
            .unwrap();

        let window = store
            .list_payments(&PaymentFilter {
                from_unix_secs: Some(150),
                to_unix_secs: Some(300),
                status: Some(PaymentStatus::Succeeded),
                ..Default::default()
            })
            .unwrap();
        let ids: Vec<&str> = window.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c"]);

        let page = store
            .list_payments(&PaymentFilter {
                limit: Some(2),
                offset: Some(1),
                ..Default::default()
            })
            .unwrap();
        let ids: Vec<&str> = page.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "d"], "oldest first, one skipped");

        let inbound = store
            .list_payments(&PaymentFilter {
                direction: Some(PaymentDirection::Inbound),
                ..Default::default()
            })
            .unwrap();
        assert!(inbound.is_empty(), "every record here is outbound");
    }

    /// A year of history must not turn into a table scan.
    #[test]
    fn a_time_window_query_uses_the_index() {
        let store = SqliteStore::in_memory().unwrap();
        let query = sql::list_payments(
            &PaymentFilter {
                from_unix_secs: Some(100),
                to_unix_secs: Some(200),
                ..Default::default()
            },
            sql::Dialect::Sqlite,
        )
        .unwrap();

        let values = query.params.into_iter().map(to_sqlite_value);
        let conn = store.lock().unwrap();
        let plan: String = conn
            .query_row(
                &format!("EXPLAIN QUERY PLAN {}", query.sql),
                params_from_iter(values),
                |row| row.get(3),
            )
            .unwrap();
        assert!(
            plan.contains("payments_created_at") || plan.contains("USING INDEX"),
            "expected an index scan, got: {plan}"
        );
    }

    /// Reopening the same file must find the data and not re-run migrations.
    #[test]
    fn data_survives_reopening_the_database() {
        let path = std::env::temp_dir().join(format!("lampo-sqlite-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let file = path.to_str().unwrap();

        {
            let store = SqliteStore::new(file).unwrap();
            store.write("ns", "", "key", b"value".to_vec()).unwrap();
            store
                .upsert_payment(&payment("a", 100, PaymentStatus::Succeeded))
                .unwrap();
        }

        let reopened = SqliteStore::new(file).unwrap();
        assert_eq!(reopened.read("ns", "", "key").unwrap(), b"value");
        assert!(reopened.get_payment("a").unwrap().is_some());

        let _ = std::fs::remove_file(&path);
    }
}
