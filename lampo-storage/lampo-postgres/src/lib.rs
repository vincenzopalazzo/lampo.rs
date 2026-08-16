//! PostgreSQL persistence backend.
//!
//! The backend for a node someone runs as a business: the same schema the
//! SQLite backend uses, but on a server that can be replicated. Lampo's part in
//! high availability ends at not acknowledging a write before `COMMIT` returns;
//! the rest is Postgres streaming replication, configured by whoever runs it.
//! There is one writer, always — two nodes on one database is split brain, and
//! for channel state that means losing money.
//!
//! # Getting sync calls onto an async client
//!
//! [`KVStoreSync`] is synchronous and `tokio_postgres` is not. Calling
//! `Runtime::block_on` from a thread that is already driving a runtime panics,
//! and LDK reaches persistence from inside lampo's runtime, so the store cannot
//! borrow the caller's runtime.
//!
//! Instead the store owns a thread with its own single-threaded runtime. That
//! thread drives the connection and serves commands sent over a channel;
//! callers hand over a command and block on the reply. No runtime is ever
//! nested, so there is nothing to deadlock.
use std::sync::mpsc as std_mpsc;
use std::thread;

use lampo_common::error;
use lampo_common::ldk::io;
use lampo_common::ldk::util::persist::KVStoreSync;
use lampo_common::persist::{
    LampoPersistenceBackend, PaymentDirection, PaymentFilter, PaymentRecord, PaymentStatus,
    PaymentStore, PersistenceKind,
};
use lampo_storage_common as sql;
use tokio::sync::mpsc;
use tokio_postgres::config::{Host, SslMode};
use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, Config, Row};
use tokio_postgres_rustls::MakeRustlsConnect;

/// Work handed to the connection thread. Each carries the channel its answer
/// goes back on.
enum Command {
    /// Run a multi-statement string through the simple-query protocol.
    Batch {
        sql: String,
        reply: std_mpsc::SyncSender<Result<(), String>>,
    },
    Execute {
        sql: String,
        params: Vec<sql::QueryParam>,
        reply: std_mpsc::SyncSender<Result<(), String>>,
    },
    ReadVersion {
        reply: std_mpsc::SyncSender<Result<i64, String>>,
    },
    KvRead {
        sql: String,
        params: Vec<sql::QueryParam>,
        reply: std_mpsc::SyncSender<Result<Option<Vec<u8>>, String>>,
    },
    KvList {
        sql: String,
        params: Vec<sql::QueryParam>,
        reply: std_mpsc::SyncSender<Result<Vec<String>, String>>,
    },
    KvListAll {
        reply: std_mpsc::SyncSender<Result<Vec<(String, String, String)>, String>>,
    },
    Payments {
        sql: String,
        params: Vec<sql::QueryParam>,
        reply: std_mpsc::SyncSender<Result<Vec<PaymentRecord>, String>>,
    },
}

/// Postgres-backed store.
pub struct PostgresStore {
    commands: mpsc::UnboundedSender<Command>,
}

/// Password-free identity of a Postgres destination, used to refuse silent
/// `storage-url` switches that would open an empty database.
pub fn destination_id(url: &str) -> error::Result<String> {
    let config = parse_config(url)?;
    let hosts = config
        .get_hosts()
        .iter()
        .map(|host| match host {
            Host::Tcp(name) => name.clone(),
            Host::Unix(path) => path.display().to_string(),
        })
        .collect::<Vec<_>>()
        .join(",");
    let ports = config
        .get_ports()
        .iter()
        .map(|port| port.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let dbname = config.get_dbname().unwrap_or("");
    let user = config.get_user().unwrap_or("");
    // `options` can change the effective schema (search_path), so two URLs that
    // share host/db but differ here are different destinations for channel state.
    let options = config.get_options().unwrap_or("");
    Ok(format!("{user}@{hosts}:{ports}/{dbname}?options={options}"))
}

impl PostgresStore {
    /// Connect to `url` (a `postgres://` connection string) and migrate.
    pub fn new(url: &str) -> error::Result<Self> {
        let config = parse_config(url)?;
        // Lock identity follows the database/schema, not the login role: two
        // users sharing public (or the same search_path) must still collide.
        let writer_lock = writer_lock_key(url)?;
        let (commands, mut rx) = mpsc::unbounded_channel::<Command>();
        let (ready_tx, ready_rx) = std_mpsc::sync_channel::<Result<(), String>>(1);

        thread::Builder::new()
            .name("lampo-postgres".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        let _ = ready_tx.send(Err(err.to_string()));
                        return;
                    }
                };

                runtime.block_on(async move {
                    let tls = tls_connector();
                    let (client, connection) = match config.connect(tls).await {
                        Ok(pair) => pair,
                        Err(err) => {
                            let _ = ready_tx.send(Err(describe(err)));
                            return;
                        }
                    };
                    // The connection future does the socket work; it has to
                    // keep being polled for any query to make progress.
                    tokio::spawn(async move {
                        if let Err(err) = connection.await {
                            log::error!(target: "lampo-postgres", "connection lost: {err}");
                        }
                    });

                    // One writer only: channel monitors cannot survive two
                    // nodes advancing the same keys independently. Hold the
                    // session lock until this connection dies.
                    match client
                        .query_one(&format!("SELECT pg_try_advisory_lock({writer_lock})"), &[])
                        .await
                    {
                        Ok(row) if row.get::<_, bool>(0) => {}
                        Ok(_) => {
                            let _ = ready_tx.send(Err(
                                "another lampo process already holds the postgres writer lock"
                                    .to_owned(),
                            ));
                            return;
                        }
                        Err(err) => {
                            let _ = ready_tx.send(Err(describe(err)));
                            return;
                        }
                    }

                    let _ = ready_tx.send(Ok(()));

                    // Awaiting here (rather than blocking) is what lets the
                    // connection task run between commands.
                    while let Some(command) = rx.recv().await {
                        serve(&client, command).await;
                    }
                });
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(err)) => error::bail!("connecting to postgres: {err}"),
            Err(err) => error::bail!("postgres thread died before connecting: {err}"),
        }

        let store = Self { commands };
        store.migrate()?;
        Ok(store)
    }

    /// Apply any migration the database has not seen yet.
    ///
    /// `CREATE TABLE IF NOT EXISTS` is not safe to run concurrently in
    /// Postgres: two sessions creating the same table collide on `pg_type`
    /// rather than one of them being a no-op. Hold an advisory lock so a second
    /// process waits for the first instead of failing to start. The lock is
    /// held on this store's own session, which is the one running the
    /// migration.
    fn migrate(&self) -> error::Result<()> {
        let lock = sql::MIGRATION_LOCK;
        self.execute(format!("SELECT pg_advisory_lock({lock})"), Vec::new())?;
        let migrated = self.migrate_locked();
        let unlocked = self.execute(format!("SELECT pg_advisory_unlock({lock})"), Vec::new());
        // Report why the migration failed in preference to why the unlock did.
        migrated.and(unlocked)
    }

    fn migrate_locked(&self) -> error::Result<()> {
        self.batch(sql::version_table(sql::Dialect::Postgres).to_owned())?;
        let applied = self.read_version()?;

        for migration in sql::migrations(sql::Dialect::Postgres) {
            if migration.version <= applied {
                continue;
            }
            // The migration file holds several statements; run it whole.
            self.batch(migration.sql.to_owned())?;
            self.execute(
                sql::write_version(sql::Dialect::Postgres).to_owned(),
                vec![sql::QueryParam::Int(migration.version)],
            )?;
            log::info!(target: "lampo-postgres", "applied schema migration {}", migration.version);
        }
        Ok(())
    }

    /// Hand a command to the connection thread and wait for its answer.
    fn send<T>(
        &self,
        build: impl FnOnce(std_mpsc::SyncSender<Result<T, String>>) -> Command,
    ) -> error::Result<T> {
        let (reply, answer) = std_mpsc::sync_channel(1);
        self.commands
            .send(build(reply))
            .map_err(|_| error::anyhow!("postgres connection thread is gone"))?;
        answer
            .recv()
            .map_err(|_| error::anyhow!("postgres connection thread dropped the reply"))?
            .map_err(|err| error::anyhow!("{err}"))
    }

    fn execute(&self, sql: String, params: Vec<sql::QueryParam>) -> error::Result<()> {
        self.send(|reply| Command::Execute { sql, params, reply })
    }

    fn batch(&self, sql: String) -> error::Result<()> {
        self.send(|reply| Command::Batch { sql, reply })
    }

    fn read_version(&self) -> error::Result<i64> {
        self.send(|reply| Command::ReadVersion { reply })
    }
}

/// Parse `url` and refuse weak TLS settings for TCP destinations.
fn parse_config(url: &str) -> error::Result<Config> {
    let mut config: Config = url
        .parse()
        .map_err(|err| error::anyhow!("invalid postgres storage-url: {err}"))?;
    let has_tcp = config
        .get_hosts()
        .iter()
        .any(|host| matches!(host, Host::Tcp(_)));
    if has_tcp {
        match config.get_ssl_mode() {
            SslMode::Require => {}
            SslMode::Disable => error::bail!(
                "postgres storage-url must not use sslmode=disable over TCP; \
                 set sslmode=require"
            ),
            // The library default is Prefer, which can silently fall back to
            // plaintext. Force Require for Lightning state over the network.
            // Also cover future SslMode variants the same way.
            _ => {
                config.ssl_mode(SslMode::Require);
            }
        }
    }
    Ok(config)
}

/// Stable advisory-lock key for the tables a connection will write.
///
/// Host/port/database/options matter; the login role does not, because two
/// roles can still share `public` (or the same `search_path`).
fn writer_lock_key(url: &str) -> error::Result<i64> {
    let config = parse_config(url)?;
    let hosts = config
        .get_hosts()
        .iter()
        .map(|host| match host {
            Host::Tcp(name) => name.clone(),
            Host::Unix(path) => path.display().to_string(),
        })
        .collect::<Vec<_>>()
        .join(",");
    let ports = config
        .get_ports()
        .iter()
        .map(|port| port.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let dbname = config.get_dbname().unwrap_or("");
    let options = config.get_options().unwrap_or("");
    let identity = format!("{hosts}:{ports}/{dbname}?options={options}");
    Ok(hash_lock_key(&identity))
}

fn hash_lock_key(identity: &str) -> i64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in identity.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let key = (hash as i64) ^ sql::WRITER_LOCK;
    if key == 0 || key == sql::MIGRATION_LOCK {
        sql::WRITER_LOCK
    } else {
        key
    }
}

/// Build a connector once per database session.
///
/// Trust Mozilla's web roots plus the host trust store, so enterprise/private
/// CAs installed on the machine work without a separate config knob. `SSL_CERT_FILE`
/// from the environment is honored by the native-certs loader.
fn tls_connector() -> MakeRustlsConnect {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_trust_anchors(webpki_roots::TLS_SERVER_ROOTS.iter().map(|anchor| {
        rustls::OwnedTrustAnchor::from_subject_spki_name_constraints(
            anchor.subject,
            anchor.spki,
            anchor.name_constraints,
        )
    }));
    match rustls_native_certs::load_native_certs() {
        Ok(certs) => {
            for cert in certs {
                if let Err(err) = roots.add(&rustls::Certificate(cert.0)) {
                    log::debug!(target: "lampo-postgres", "skipping native CA: {err:?}");
                }
            }
        }
        Err(err) => {
            log::warn!(target: "lampo-postgres", "loading native CA certificates: {err}");
        }
    }
    let config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(roots)
        .with_no_client_auth();
    MakeRustlsConnect::new(config)
}

/// Postgres errors display as a bare "db error"; the statement that failed and
/// the reason are in the source, so a log line without it says nothing.
fn describe(err: tokio_postgres::Error) -> String {
    use std::error::Error;
    match err.source() {
        Some(source) => format!("{err}: {source}"),
        None => err.to_string(),
    }
}

/// Run one command against the client and answer it.
async fn serve(client: &Client, command: Command) {
    match command {
        Command::Batch { sql, reply } => {
            let result = client.batch_execute(&sql).await.map_err(describe);
            let _ = reply.send(result);
        }
        Command::Execute { sql, params, reply } => {
            let bound = bind(&params);
            let result = client
                .execute(&sql, &slice(&bound))
                .await
                .map(|_| ())
                .map_err(describe);
            let _ = reply.send(result);
        }
        Command::ReadVersion { reply } => {
            let result = client
                .query_opt(sql::read_version(), &[])
                .await
                .map(|row| row.map(|row| row.get::<_, i64>(0)).unwrap_or(0))
                .map_err(describe);
            let _ = reply.send(result);
        }
        Command::KvRead { sql, params, reply } => {
            let bound = bind(&params);
            let result = client
                .query_opt(&sql, &slice(&bound))
                .await
                .map(|row| row.map(|row| row.get::<_, Vec<u8>>(0)))
                .map_err(describe);
            let _ = reply.send(result);
        }
        Command::KvList { sql, params, reply } => {
            let bound = bind(&params);
            let result = client
                .query(&sql, &slice(&bound))
                .await
                .map(|rows| rows.iter().map(|row| row.get::<_, String>(0)).collect())
                .map_err(describe);
            let _ = reply.send(result);
        }
        Command::KvListAll { reply } => {
            let result = client
                .query(sql::kv_list_all(), &[])
                .await
                .map(|rows| {
                    rows.iter()
                        .map(|row| (row.get(0), row.get(1), row.get(2)))
                        .collect()
                })
                .map_err(describe);
            let _ = reply.send(result);
        }
        Command::Payments { sql, params, reply } => {
            let bound = bind(&params);
            let result = match client.query(&sql, &slice(&bound)).await {
                Ok(rows) => rows
                    .iter()
                    .map(row_to_payment)
                    .collect::<error::Result<Vec<_>>>()
                    .map_err(|err| err.to_string()),
                Err(err) => Err(describe(err)),
            };
            let _ = reply.send(result);
        }
    }
}

/// Owned values for the bound parameters, kept alive while the query runs.
fn bind(params: &[sql::QueryParam]) -> Vec<Box<dyn ToSql + Sync + Send>> {
    params
        .iter()
        .map(|param| match param {
            sql::QueryParam::Int(value) => Box::new(*value) as Box<dyn ToSql + Sync + Send>,
            sql::QueryParam::Text(value) => Box::new(value.clone()) as Box<dyn ToSql + Sync + Send>,
            sql::QueryParam::Bytes(value) => {
                Box::new(value.clone()) as Box<dyn ToSql + Sync + Send>
            }
            sql::QueryParam::NullInt => {
                Box::new(Option::<i64>::None) as Box<dyn ToSql + Sync + Send>
            }
            sql::QueryParam::NullText => {
                Box::new(Option::<String>::None) as Box<dyn ToSql + Sync + Send>
            }
        })
        .collect()
}

fn slice<'a>(bound: &'a [Box<dyn ToSql + Sync + Send>]) -> Vec<&'a (dyn ToSql + Sync)> {
    bound.iter().map(|value| value.as_ref() as _).collect()
}

fn row_to_payment(row: &Row) -> error::Result<PaymentRecord> {
    let amount_msat: i64 = row.get(3);
    let fee_msat: Option<i64> = row.get(4);
    let created_at: i64 = row.get(6);
    Ok(PaymentRecord {
        id: row.get(0),
        payment_hash: row.get(1),
        direction: PaymentDirection::from_str(row.get(2))?,
        amount_msat: amount_msat as u64,
        fee_msat: fee_msat.map(|fee| fee as u64),
        status: PaymentStatus::from_str(row.get(5))?,
        created_at: created_at as u64,
        invoice: row.get(7),
    })
}

/// Amounts are unsigned in lampo and signed in SQL; no real amount is anywhere
/// near the boundary, so a value that does not fit is a bug worth reporting.
fn to_column(value: u64, what: &str) -> error::Result<i64> {
    value
        .try_into()
        .map_err(|_| error::anyhow!("{what} {value} does not fit a 64-bit column"))
}

fn io_err(err: error::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, err.to_string())
}

impl KVStoreSync for PostgresStore {
    fn read(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
    ) -> Result<Vec<u8>, io::Error> {
        let params = vec![
            sql::QueryParam::Text(primary_namespace.to_owned()),
            sql::QueryParam::Text(secondary_namespace.to_owned()),
            sql::QueryParam::Text(key.to_owned()),
        ];
        let value = self
            .send(|reply| Command::KvRead {
                sql: sql::kv_read(sql::Dialect::Postgres),
                params,
                reply,
            })
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
        let params = vec![
            sql::QueryParam::Text(primary_namespace.to_owned()),
            sql::QueryParam::Text(secondary_namespace.to_owned()),
            sql::QueryParam::Text(key.to_owned()),
            sql::QueryParam::Bytes(buf),
        ];
        self.execute(sql::kv_write(sql::Dialect::Postgres).to_owned(), params)
            .map_err(io_err)
    }

    fn remove(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
        key: &str,
        _lazy: bool,
    ) -> Result<(), io::Error> {
        let params = vec![
            sql::QueryParam::Text(primary_namespace.to_owned()),
            sql::QueryParam::Text(secondary_namespace.to_owned()),
            sql::QueryParam::Text(key.to_owned()),
        ];
        self.execute(sql::kv_remove(sql::Dialect::Postgres), params)
            .map_err(io_err)
    }

    fn list(
        &self,
        primary_namespace: &str,
        secondary_namespace: &str,
    ) -> Result<Vec<String>, io::Error> {
        let params = vec![
            sql::QueryParam::Text(primary_namespace.to_owned()),
            sql::QueryParam::Text(secondary_namespace.to_owned()),
        ];
        self.send(|reply| Command::KvList {
            sql: sql::kv_list(sql::Dialect::Postgres),
            params,
            reply,
        })
        .map_err(io_err)
    }
}

impl PaymentStore for PostgresStore {
    fn upsert_payment(&self, payment: &PaymentRecord) -> error::Result<()> {
        let params = vec![
            sql::QueryParam::Text(payment.id.clone()),
            sql::QueryParam::Text(payment.payment_hash.clone()),
            sql::QueryParam::Text(payment.direction.as_str().to_owned()),
            sql::QueryParam::Int(to_column(payment.amount_msat, "amount_msat")?),
            match payment.fee_msat {
                Some(fee) => sql::QueryParam::Int(to_column(fee, "fee_msat")?),
                None => sql::QueryParam::NullInt,
            },
            sql::QueryParam::Text(payment.status.as_str().to_owned()),
            sql::QueryParam::Int(to_column(payment.created_at, "created_at")?),
            match &payment.invoice {
                Some(invoice) => sql::QueryParam::Text(invoice.clone()),
                None => sql::QueryParam::NullText,
            },
        ];
        self.execute(
            sql::payment_write(sql::Dialect::Postgres).to_owned(),
            params,
        )
    }

    fn get_payment(&self, id: &str) -> error::Result<Option<PaymentRecord>> {
        let params = vec![sql::QueryParam::Text(id.to_owned())];
        let mut found = self.send(|reply| Command::Payments {
            sql: sql::payment_read(sql::Dialect::Postgres),
            params,
            reply,
        })?;
        Ok(found.pop())
    }

    fn list_payments(&self, filter: &PaymentFilter) -> error::Result<Vec<PaymentRecord>> {
        let query = sql::list_payments(filter, sql::Dialect::Postgres)?;
        self.send(|reply| Command::Payments {
            sql: query.sql,
            params: query.params,
            reply,
        })
    }
}

impl LampoPersistenceBackend for PostgresStore {
    fn kind(&self) -> PersistenceKind {
        PersistenceKind::Postgres
    }

    fn list_all_keys(&self) -> error::Result<Vec<(String, String, String)>> {
        self.send(|reply| Command::KvListAll { reply })
    }
}

/// These need a real server, so they run only when `LAMPO_TEST_POSTGRES_URL`
/// points at one, for example:
///
/// ```text
/// docker run --rm -d -p 5433:5432 -e POSTGRES_PASSWORD=lampo --name lampo-pg postgres:16
/// LAMPO_TEST_POSTGRES_URL=postgres://postgres:lampo@127.0.0.1:5433/postgres cargo test -p lampo-postgres
/// ```
#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use super::*;

    /// Connect to the test server, or skip. Each store gets its own schema so
    /// tests do not collide.
    fn store() -> Option<Arc<dyn LampoPersistenceBackend>> {
        let url = std::env::var("LAMPO_TEST_POSTGRES_URL").ok()?;
        static NONCE: AtomicU32 = AtomicU32::new(0);
        let schema = format!(
            "lampo_test_{}_{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        );

        // Drop the setup connection before opening the scoped store so its
        // writer lock is released. The scoped URL includes search_path, so it
        // takes a different destination lock either way.
        {
            let setup = PostgresStore::new(&url).expect("connecting to the test server");
            setup
                .execute(
                    format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
                    Vec::new(),
                )
                .unwrap();
            setup
                .execute(format!("CREATE SCHEMA {schema}"), Vec::new())
                .unwrap();
        }

        // `options` puts every statement of this connection in that schema, so
        // the migrations build the tables there.
        let scoped = format!("{url}?options=-csearch_path%3D{schema}");
        Some(Arc::new(
            PostgresStore::new(&scoped).expect("opening the scoped store"),
        ))
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
    fn kv_round_trips_and_reports_its_kind() {
        let Some(store) = store() else {
            eprintln!("skipped: set LAMPO_TEST_POSTGRES_URL to run");
            return;
        };
        assert_eq!(store.kind(), PersistenceKind::Postgres);

        store.write("ns", "sub", "key", b"value".to_vec()).unwrap();
        assert_eq!(store.read("ns", "sub", "key").unwrap(), b"value");
        assert_eq!(store.list("ns", "sub").unwrap(), vec!["key".to_string()]);

        store.write("ns", "sub", "key", b"newer".to_vec()).unwrap();
        assert_eq!(store.read("ns", "sub", "key").unwrap(), b"newer");

        store.remove("ns", "sub", "key", false).unwrap();
        assert!(store.list("ns", "sub").unwrap().is_empty());
    }

    /// `payer_proof::load` maps this exact error kind to "no proof stored".
    #[test]
    fn reading_a_missing_key_reports_not_found() {
        let Some(store) = store() else {
            eprintln!("skipped: set LAMPO_TEST_POSTGRES_URL to run");
            return;
        };
        let err = store.read("ns", "", "absent").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn payments_round_trip_filter_and_paginate() {
        let Some(store) = store() else {
            eprintln!("skipped: set LAMPO_TEST_POSTGRES_URL to run");
            return;
        };
        for (id, at) in [("a", 100), ("b", 200), ("c", 300)] {
            store
                .upsert_payment(&payment(id, at, PaymentStatus::Succeeded))
                .unwrap();
        }
        store
            .upsert_payment(&payment("d", 250, PaymentStatus::Failed))
            .unwrap();

        assert_eq!(
            store.get_payment("a").unwrap().unwrap(),
            payment("a", 100, PaymentStatus::Succeeded)
        );
        assert!(store.get_payment("absent").unwrap().is_none());

        // Upsert replaces rather than appending.
        store
            .upsert_payment(&payment("a", 100, PaymentStatus::Failed))
            .unwrap();
        assert_eq!(
            store
                .list_payments(&PaymentFilter::default())
                .unwrap()
                .len(),
            4
        );

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
    }

    /// A record with no fee and no invoice has to survive the null columns.
    #[test]
    fn payments_with_empty_columns_round_trip() {
        let Some(store) = store() else {
            eprintln!("skipped: set LAMPO_TEST_POSTGRES_URL to run");
            return;
        };
        let bare = PaymentRecord {
            fee_msat: None,
            invoice: None,
            direction: PaymentDirection::Inbound,
            ..payment("bare", 10, PaymentStatus::Pending)
        };
        store.upsert_payment(&bare).unwrap();
        assert_eq!(store.get_payment("bare").unwrap().unwrap(), bare);
    }

    #[test]
    fn destination_id_includes_session_options() {
        let base = "postgres://lampo@127.0.0.1:5432/lampo?sslmode=require";
        let plain = destination_id(base).unwrap();
        let scoped = destination_id(&format!("{base}&options=-csearch_path%3Dother")).unwrap();
        assert!(
            plain != scoped,
            "search_path must change the destination fingerprint"
        );
        assert!(
            scoped.contains("options=-csearch_path=other")
                || scoped.contains("options=-csearch_path%3Dother")
                || scoped.contains("search_path"),
            "fingerprint should carry the options: {scoped}"
        );
        assert_ne!(
            writer_lock_key(base).unwrap(),
            writer_lock_key(&format!("{base}&options=-csearch_path%3Dother")).unwrap(),
            "different schemas must not share the writer lock"
        );
        let other_user = "postgres://other@127.0.0.1:5432/lampo?sslmode=require";
        assert_eq!(
            writer_lock_key(base).unwrap(),
            writer_lock_key(other_user).unwrap(),
            "login role must not change the writer lock for the same schema"
        );
    }
}
