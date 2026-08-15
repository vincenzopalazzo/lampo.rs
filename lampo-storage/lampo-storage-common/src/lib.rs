//! SQL shared by lampo's database backends.
//!
//! SQLite and Postgres differ in type names and parameter syntax but not in
//! what lampo stores, so the schema, the migrations and the payment queries are
//! written once here and rendered per dialect. Keeping them in one crate is
//! what stops the two backends from drifting into different databases.
use lampo_common::error;
use lampo_common::persist::PaymentFilter;

/// Table holding the key/value half of a backend: LDK's blobs.
pub const KV_TABLE: &str = "kv";
/// Table holding lampo's payment records.
pub const PAYMENTS_TABLE: &str = "payments";
/// Single-row table tracking which migrations have been applied.
pub const VERSION_TABLE: &str = "schema_version";

/// Which SQL flavour to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
}

impl Dialect {
    /// Type for an opaque byte string.
    fn blob(&self) -> &'static str {
        match self {
            Self::Sqlite => "BLOB",
            Self::Postgres => "BYTEA",
        }
    }

    /// Type for a 64-bit integer. SQLite's INTEGER is already 64-bit.
    fn int8(&self) -> &'static str {
        match self {
            Self::Sqlite => "INTEGER",
            Self::Postgres => "BIGINT",
        }
    }

    /// Placeholder for the `n`th bound parameter, counting from one.
    ///
    /// SQLite takes positional `?`, Postgres numbered `$n`.
    pub fn placeholder(&self, n: usize) -> String {
        match self {
            Self::Sqlite => "?".to_owned(),
            Self::Postgres => format!("${n}"),
        }
    }
}

/// A schema change, applied in one transaction and recorded by `version`.
pub struct Migration {
    pub version: i64,
    pub statements: Vec<String>,
}

/// Every migration, oldest first.
///
/// Migrations are append-only: to change the schema add a new entry, never edit
/// an existing one, or nodes that already ran it will disagree with new ones.
pub fn migrations(dialect: Dialect) -> Vec<Migration> {
    vec![Migration {
        version: 1,
        statements: vec![
            format!(
                "CREATE TABLE IF NOT EXISTS {KV_TABLE} (
                     primary_namespace   TEXT NOT NULL,
                     secondary_namespace TEXT NOT NULL,
                     key                 TEXT NOT NULL,
                     value               {} NOT NULL,
                     PRIMARY KEY (primary_namespace, secondary_namespace, key)
                 )",
                dialect.blob()
            ),
            format!(
                "CREATE TABLE IF NOT EXISTS {PAYMENTS_TABLE} (
                     id           TEXT PRIMARY KEY,
                     payment_hash TEXT NOT NULL,
                     direction    TEXT NOT NULL,
                     amount_msat  {int} NOT NULL,
                     fee_msat     {int},
                     status       TEXT NOT NULL,
                     created_at   {int} NOT NULL,
                     invoice      TEXT
                 )",
                int = dialect.int8()
            ),
            // The whole reason payments are not in the key/value table: these
            // turn "every payment in a window" into an index scan.
            format!(
                "CREATE INDEX IF NOT EXISTS payments_created_at
                 ON {PAYMENTS_TABLE} (created_at)"
            ),
            format!(
                "CREATE INDEX IF NOT EXISTS payments_status_created_at
                 ON {PAYMENTS_TABLE} (status, created_at)"
            ),
        ],
    }]
}

/// Statement creating the table that records the applied schema version.
pub fn version_table(dialect: Dialect) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {VERSION_TABLE} (
             id      {int} PRIMARY KEY,
             version {int} NOT NULL
         )",
        int = dialect.int8()
    )
}

/// Read the applied schema version, zero when nothing has run yet.
pub fn read_version() -> String {
    format!("SELECT version FROM {VERSION_TABLE} WHERE id = 1")
}

/// Record `version` as applied.
pub fn write_version(dialect: Dialect) -> String {
    match dialect {
        Dialect::Sqlite => {
            format!("INSERT OR REPLACE INTO {VERSION_TABLE} (id, version) VALUES (1, ?)")
        }
        Dialect::Postgres => format!(
            "INSERT INTO {VERSION_TABLE} (id, version) VALUES (1, $1)
             ON CONFLICT (id) DO UPDATE SET version = EXCLUDED.version"
        ),
    }
}

/// Upsert a key/value entry.
pub fn kv_write(dialect: Dialect) -> String {
    let (p1, p2, p3, p4) = (
        dialect.placeholder(1),
        dialect.placeholder(2),
        dialect.placeholder(3),
        dialect.placeholder(4),
    );
    match dialect {
        Dialect::Sqlite => format!(
            "INSERT OR REPLACE INTO {KV_TABLE}
                 (primary_namespace, secondary_namespace, key, value)
             VALUES ({p1}, {p2}, {p3}, {p4})"
        ),
        Dialect::Postgres => format!(
            "INSERT INTO {KV_TABLE}
                 (primary_namespace, secondary_namespace, key, value)
             VALUES ({p1}, {p2}, {p3}, {p4})
             ON CONFLICT (primary_namespace, secondary_namespace, key)
             DO UPDATE SET value = EXCLUDED.value"
        ),
    }
}

pub fn kv_read(dialect: Dialect) -> String {
    format!(
        "SELECT value FROM {KV_TABLE}
         WHERE primary_namespace = {} AND secondary_namespace = {} AND key = {}",
        dialect.placeholder(1),
        dialect.placeholder(2),
        dialect.placeholder(3),
    )
}

pub fn kv_remove(dialect: Dialect) -> String {
    format!(
        "DELETE FROM {KV_TABLE}
         WHERE primary_namespace = {} AND secondary_namespace = {} AND key = {}",
        dialect.placeholder(1),
        dialect.placeholder(2),
        dialect.placeholder(3),
    )
}

pub fn kv_list(dialect: Dialect) -> String {
    format!(
        "SELECT key FROM {KV_TABLE}
         WHERE primary_namespace = {} AND secondary_namespace = {}",
        dialect.placeholder(1),
        dialect.placeholder(2),
    )
}

/// Upsert a payment record. Columns are in [`PAYMENT_COLUMNS`] order.
pub fn payment_write(dialect: Dialect) -> String {
    let values = (1..=8)
        .map(|n| dialect.placeholder(n))
        .collect::<Vec<_>>()
        .join(", ");
    match dialect {
        Dialect::Sqlite => {
            format!("INSERT OR REPLACE INTO {PAYMENTS_TABLE} ({PAYMENT_COLUMNS}) VALUES ({values})")
        }
        Dialect::Postgres => format!(
            "INSERT INTO {PAYMENTS_TABLE} ({PAYMENT_COLUMNS}) VALUES ({values})
             ON CONFLICT (id) DO UPDATE SET
                 payment_hash = EXCLUDED.payment_hash,
                 direction    = EXCLUDED.direction,
                 amount_msat  = EXCLUDED.amount_msat,
                 fee_msat     = EXCLUDED.fee_msat,
                 status       = EXCLUDED.status,
                 created_at   = EXCLUDED.created_at,
                 invoice      = EXCLUDED.invoice"
        ),
    }
}

/// Payment columns, in the order both backends read and write them.
pub const PAYMENT_COLUMNS: &str =
    "id, payment_hash, direction, amount_msat, fee_msat, status, created_at, invoice";

pub fn payment_read(dialect: Dialect) -> String {
    format!(
        "SELECT {PAYMENT_COLUMNS} FROM {PAYMENTS_TABLE} WHERE id = {}",
        dialect.placeholder(1)
    )
}

/// A parameter bound to a rendered query, in the order it appears.
///
/// Nulls carry the column type they stand in for: Postgres binds a parameter by
/// type, so an untyped null against a `BIGINT` column is rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryParam {
    Int(i64),
    Text(String),
    Bytes(Vec<u8>),
    NullInt,
    NullText,
}

/// A rendered query and the parameters to bind to it.
#[derive(Debug, Clone)]
pub struct Query {
    pub sql: String,
    pub params: Vec<QueryParam>,
}

/// Render [`PaymentFilter`] as SQL, so the database does the filtering.
///
/// Ordering is oldest first, matching the trait, and `created_at` is indexed so
/// a time window is a range scan rather than a table scan. Errors only if a
/// filter bound does not fit a signed 64-bit column.
pub fn list_payments(filter: &PaymentFilter, dialect: Dialect) -> error::Result<Query> {
    let mut conditions: Vec<String> = Vec::new();
    let mut params: Vec<QueryParam> = Vec::new();

    /// Bound values are `u64` in the filter but the columns are signed, so a
    /// value past `i64::MAX` is a caller error rather than something to wrap.
    fn as_column_int(value: u64, what: &str) -> error::Result<i64> {
        value
            .try_into()
            .map_err(|_| error::anyhow!("{what} {value} does not fit a 64-bit column"))
    }

    if let Some(from) = filter.from_unix_secs {
        params.push(QueryParam::Int(as_column_int(from, "from_unix_secs")?));
        conditions.push(format!(
            "created_at >= {}",
            dialect.placeholder(params.len())
        ));
    }
    if let Some(to) = filter.to_unix_secs {
        params.push(QueryParam::Int(as_column_int(to, "to_unix_secs")?));
        conditions.push(format!(
            "created_at <= {}",
            dialect.placeholder(params.len())
        ));
    }
    if let Some(direction) = filter.direction {
        params.push(QueryParam::Text(direction.as_str().to_owned()));
        conditions.push(format!("direction = {}", dialect.placeholder(params.len())));
    }
    if let Some(status) = filter.status {
        params.push(QueryParam::Text(status.as_str().to_owned()));
        conditions.push(format!("status = {}", dialect.placeholder(params.len())));
    }

    let mut sql = format!("SELECT {PAYMENT_COLUMNS} FROM {PAYMENTS_TABLE}");
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    // Ties on created_at would otherwise paginate unpredictably.
    sql.push_str(" ORDER BY created_at ASC, id ASC");

    // An OFFSET without a LIMIT is not portable, so stand in the largest limit
    // the column takes when only an offset was asked for.
    if let Some(limit) = filter.limit {
        params.push(QueryParam::Int(as_column_int(limit, "limit")?));
        sql.push_str(&format!(" LIMIT {}", dialect.placeholder(params.len())));
    } else if filter.offset.is_some() {
        params.push(QueryParam::Int(i64::MAX));
        sql.push_str(&format!(" LIMIT {}", dialect.placeholder(params.len())));
    }
    if let Some(offset) = filter.offset {
        params.push(QueryParam::Int(as_column_int(offset, "offset")?));
        sql.push_str(&format!(" OFFSET {}", dialect.placeholder(params.len())));
    }

    Ok(Query { sql, params })
}

#[cfg(test)]
mod tests {
    use lampo_common::persist::{PaymentDirection, PaymentStatus};

    use super::*;

    #[test]
    fn dialects_render_their_own_placeholders() {
        assert_eq!(Dialect::Sqlite.placeholder(3), "?");
        assert_eq!(Dialect::Postgres.placeholder(3), "$3");
    }

    #[test]
    fn both_dialects_define_the_same_tables() {
        for dialect in [Dialect::Sqlite, Dialect::Postgres] {
            let sql = migrations(dialect)
                .into_iter()
                .flat_map(|migration| migration.statements)
                .collect::<Vec<_>>()
                .join(";");
            assert!(
                sql.contains(KV_TABLE),
                "{dialect:?} is missing the kv table"
            );
            assert!(
                sql.contains(PAYMENTS_TABLE),
                "{dialect:?} is missing the payments table"
            );
            assert!(
                sql.contains("payments_created_at"),
                "{dialect:?} is missing the time index, which is the point of the table"
            );
        }
    }

    #[test]
    fn an_empty_filter_selects_everything_in_order() {
        let query = list_payments(&PaymentFilter::default(), Dialect::Sqlite).unwrap();
        assert!(!query.sql.contains("WHERE"), "{}", query.sql);
        assert!(
            query.sql.contains("ORDER BY created_at ASC"),
            "{}",
            query.sql
        );
        assert!(query.params.is_empty());
    }

    #[test]
    fn filters_become_numbered_parameters_for_postgres() {
        let query = list_payments(
            &PaymentFilter {
                from_unix_secs: Some(100),
                to_unix_secs: Some(200),
                direction: Some(PaymentDirection::Outbound),
                status: Some(PaymentStatus::Succeeded),
                ..Default::default()
            },
            Dialect::Postgres,
        )
        .unwrap();

        assert!(query.sql.contains("created_at >= $1"), "{}", query.sql);
        assert!(query.sql.contains("created_at <= $2"), "{}", query.sql);
        assert!(query.sql.contains("direction = $3"), "{}", query.sql);
        assert!(query.sql.contains("status = $4"), "{}", query.sql);
        assert_eq!(
            query.params,
            vec![
                QueryParam::Int(100),
                QueryParam::Int(200),
                QueryParam::Text("outbound".to_owned()),
                QueryParam::Text("succeeded".to_owned()),
            ]
        );
    }

    /// An OFFSET with no LIMIT is rejected by SQLite, so one has to be supplied.
    #[test]
    fn an_offset_without_a_limit_still_renders_a_limit() {
        let query = list_payments(
            &PaymentFilter {
                offset: Some(5),
                ..Default::default()
            },
            Dialect::Sqlite,
        )
        .unwrap();
        assert!(query.sql.contains("LIMIT"), "{}", query.sql);
        assert!(query.sql.contains("OFFSET"), "{}", query.sql);
        assert_eq!(
            query.params,
            vec![QueryParam::Int(i64::MAX), QueryParam::Int(5)]
        );
    }

    #[test]
    fn limit_and_offset_bind_in_order() {
        let query = list_payments(
            &PaymentFilter {
                status: Some(PaymentStatus::Failed),
                limit: Some(10),
                offset: Some(20),
                ..Default::default()
            },
            Dialect::Postgres,
        )
        .unwrap();
        assert!(query.sql.contains("status = $1"), "{}", query.sql);
        assert!(query.sql.contains("LIMIT $2"), "{}", query.sql);
        assert!(query.sql.contains("OFFSET $3"), "{}", query.sql);
        assert_eq!(
            query.params,
            vec![
                QueryParam::Text("failed".to_owned()),
                QueryParam::Int(10),
                QueryParam::Int(20),
            ]
        );
    }
}
