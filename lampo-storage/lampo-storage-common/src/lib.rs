//! SQL shared by lampo's database backends.
//!
//! The statements live in `.sql` files under `sql/`, embedded at compile time:
//! `sql/sqlite/` and `sql/postgres/` hold what genuinely differs between the
//! dialects (types, upsert syntax), and `sql/common/` holds the queries that
//! only differ in placeholder style, written in `$n` form and rendered to
//! SQLite's `?n` on load. Keeping one home for the SQL is what stops the two
//! backends from drifting into different databases.
//!
//! The one query still assembled in Rust is [`list_payments`], because the
//! filter is dynamic.
use lampo_common::error;
use lampo_common::persist::PaymentFilter;

/// Advisory lock key held while migrating a shared server (Postgres).
/// Arbitrary, but it has to be the same in every lampo build and tool touching
/// the same database, or they would not exclude each other.
pub const MIGRATION_LOCK: i64 = 0x6c_61_6d_70_6f_00; // "lampo"

/// Payment columns, in the order every backend reads and writes them.
pub const PAYMENT_COLUMNS: &str =
    "id, payment_hash, direction, amount_msat, fee_msat, status, created_at, invoice";

/// Which SQL flavour to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
}

impl Dialect {
    /// Placeholder for the `n`th bound parameter, counting from one.
    pub fn placeholder(&self, n: usize) -> String {
        match self {
            Self::Sqlite => format!("?{n}"),
            Self::Postgres => format!("${n}"),
        }
    }

    /// Render a `sql/common/` query (written in `$n` form) for this dialect.
    ///
    /// SQLite takes `?n`; none of the shared queries contain a literal `$`, so
    /// the swap is mechanical.
    fn render(&self, sql: &'static str) -> String {
        match self {
            Self::Sqlite => sql.replace('$', "?"),
            Self::Postgres => sql.to_owned(),
        }
    }
}

/// A schema change, applied in one transaction where the backend supports it
/// and recorded by `version`.
pub struct Migration {
    pub version: i64,
    /// The whole migration file; may hold several `;`-separated statements.
    pub sql: &'static str,
}

/// Every migration, oldest first.
///
/// Migrations are append-only: to change the schema add a new file and a new
/// entry, never edit an existing one, or nodes that already ran it will
/// disagree with new ones.
pub fn migrations(dialect: Dialect) -> Vec<Migration> {
    match dialect {
        Dialect::Sqlite => vec![Migration {
            version: 1,
            sql: include_str!("../sql/sqlite/0001_init.sql"),
        }],
        Dialect::Postgres => vec![Migration {
            version: 1,
            sql: include_str!("../sql/postgres/0001_init.sql"),
        }],
    }
}

/// Statement creating the table that records the applied schema version.
pub fn version_table(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Sqlite => include_str!("../sql/sqlite/version_table.sql"),
        Dialect::Postgres => include_str!("../sql/postgres/version_table.sql"),
    }
}

/// Read the applied schema version, no rows when nothing has run yet.
pub fn read_version() -> &'static str {
    include_str!("../sql/common/version_read.sql")
}

/// Record a version as applied.
pub fn write_version(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Sqlite => include_str!("../sql/sqlite/version_write.sql"),
        Dialect::Postgres => include_str!("../sql/postgres/version_write.sql"),
    }
}

/// Upsert a key/value entry.
pub fn kv_write(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Sqlite => include_str!("../sql/sqlite/kv_write.sql"),
        Dialect::Postgres => include_str!("../sql/postgres/kv_write.sql"),
    }
}

pub fn kv_read(dialect: Dialect) -> String {
    dialect.render(include_str!("../sql/common/kv_read.sql"))
}

pub fn kv_remove(dialect: Dialect) -> String {
    dialect.render(include_str!("../sql/common/kv_remove.sql"))
}

pub fn kv_list(dialect: Dialect) -> String {
    dialect.render(include_str!("../sql/common/kv_list.sql"))
}

/// Every `(primary_namespace, secondary_namespace, key)` in the store, for
/// backends implementing `list_all_keys`.
pub fn kv_list_all() -> &'static str {
    include_str!("../sql/common/kv_list_all.sql")
}

/// Upsert a payment record, columns in [`PAYMENT_COLUMNS`] order.
pub fn payment_write(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::Sqlite => include_str!("../sql/sqlite/payment_write.sql"),
        Dialect::Postgres => include_str!("../sql/postgres/payment_write.sql"),
    }
}

pub fn payment_read(dialect: Dialect) -> String {
    dialect.render(include_str!("../sql/common/payment_read.sql"))
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

    let mut sql = format!("SELECT {PAYMENT_COLUMNS} FROM payments");
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
        assert_eq!(Dialect::Sqlite.placeholder(3), "?3");
        assert_eq!(Dialect::Postgres.placeholder(3), "$3");
    }

    /// The shared queries are written in `$n` form; the SQLite rendering must
    /// leave no `$` behind.
    #[test]
    fn shared_queries_render_for_both_dialects() {
        for query in [kv_read, kv_remove, kv_list, payment_read] {
            assert!(query(Dialect::Postgres).contains("$1"));
            let sqlite = query(Dialect::Sqlite);
            assert!(!sqlite.contains('$'), "{sqlite}");
            assert!(sqlite.contains("?1"), "{sqlite}");
        }
    }

    #[test]
    fn both_dialects_define_the_same_tables() {
        for dialect in [Dialect::Sqlite, Dialect::Postgres] {
            let sql = migrations(dialect)
                .into_iter()
                .map(|migration| migration.sql)
                .collect::<Vec<_>>()
                .join(";");
            assert!(sql.contains("kv"), "{dialect:?} is missing the kv table");
            assert!(
                sql.contains("payments"),
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
