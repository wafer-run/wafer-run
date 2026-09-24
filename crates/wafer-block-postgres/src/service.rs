use std::{
    collections::HashMap,
    str::FromStr as _,
    sync::atomic::{AtomicBool, Ordering},
};

use base64ct::{Base64, Encoding};
use sqlx::{
    pool::PoolConnection,
    postgres::{PgConnectOptions, PgConnection, PgRow},
    ConnectOptions as _, PgPool, Postgres, Row,
};
#[cfg(test)]
use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_block_macro::wafer_async_trait;
use wafer_core::{
    forward_database_service,
    interfaces::database::{
        codec::{self, JsonColumns},
        exec::{DbExec, TxOp, TxResult},
        schema_cache::SchemaCache,
        service::{Column, DatabaseError, Record},
    },
};
#[cfg(test)]
use wafer_sql_utils::value::sea_values_to_json;
use wafer_sql_utils::{ddl, introspect, Backend};

use crate::{errors::sqlx_error, params};

/// PostgreSQL implementation of the DatabaseService.
///
/// Uses `sqlx` with connection pooling.
pub struct PostgresDatabaseService {
    pool: PgPool,
    /// Memoized table-exists / column-list facts (see [`SchemaCache`]).
    /// Invalidated on every schema mutation this service performs.
    schema_cache: SchemaCache,
    /// STRICT_SCHEMA flag; applied once at lifecycle `Init` via
    /// [`DatabaseService::set_strict_schema`]. When set, the shared executor
    /// skips schema introspection entirely.
    strict_schema: AtomicBool,
}

impl PostgresDatabaseService {
    /// Connect to a PostgreSQL database using a connection URL.
    ///
    /// A URL that turns the statement cache off (`statement-cache-capacity=0`)
    /// is refused before connecting; see [`from_pool`](Self::from_pool).
    pub async fn connect(url: &str) -> Result<Self, DatabaseError> {
        let options = PgConnectOptions::from_str(url).map_err(|e| sqlx_error(&e))?;
        require_statement_cache(&options)?;
        let pool = PgPool::connect_with(options)
            .await
            .map_err(|e| sqlx_error(&e))?;
        Self::from_pool(pool)
    }

    /// Create a service from an existing connection pool.
    ///
    /// The pool's connections must keep a statement cache (sqlx's default of
    /// 100; not `statement-cache-capacity=0`): every statement is prepared
    /// once to learn its parameter types and then run as that cached
    /// statement (see [`params::bind`]). Without the cache each prepare leaves
    /// a named statement on the server connection that nothing closes, so a
    /// pool configured that way is refused with `InvalidArgument`.
    pub fn from_pool(pool: PgPool) -> Result<Self, DatabaseError> {
        require_statement_cache(&pool.connect_options())?;
        Ok(Self {
            pool,
            schema_cache: SchemaCache::new(),
            strict_schema: AtomicBool::new(false),
        })
    }

    /// A pooled connection; a statement's arguments are encoded for, and it
    /// is executed on, one connection (see [`params::bind`]).
    async fn connection(&self) -> Result<PoolConnection<Postgres>, DatabaseError> {
        self.pool.acquire().await.map_err(|e| sqlx_error(&e))
    }

    // -----------------------------------------------------------------
    // Schema DDL async helpers
    // -----------------------------------------------------------------

    async fn schema_drop_table_async(&self, name: &str) -> Result<(), DatabaseError> {
        let stmt = ddl::build_drop_table(name, Backend::Postgres)?;
        sqlx::query(&stmt.sql)
            .execute(&self.pool)
            .await
            .map_err(|e| sqlx_error(&e))?;
        Ok(())
    }

    async fn schema_add_column_async(
        &self,
        table: &str,
        column: &Column,
    ) -> Result<(), DatabaseError> {
        let stmt = ddl::build_add_column(table, column, Backend::Postgres)?;
        sqlx::query(&stmt.sql)
            .execute(&self.pool)
            .await
            .map_err(|e| sqlx_error(&e))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Trait implementation — direct async
// ---------------------------------------------------------------------------

#[wafer_async_trait]
impl DbExec for PostgresDatabaseService {
    const BACKEND: Backend = Backend::Postgres;

    fn schema_cache(&self) -> Option<&SchemaCache> {
        Some(&self.schema_cache)
    }

    fn strict_schema(&self) -> bool {
        self.strict_schema.load(Ordering::Relaxed)
    }

    async fn run_fetch(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        _json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError> {
        let mut conn = self.connection().await?;
        let rows = fetch_all(&mut conn, sql, params).await?;
        rows.iter().map(row_to_record).collect()
    }

    async fn run_fetch_one(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        _json: &JsonColumns,
    ) -> Result<Record, DatabaseError> {
        let mut conn = self.connection().await?;
        let args = params::bind(&mut conn, sql, params).await?;
        let row = sqlx::query_with(sql, args)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| sqlx_error(&e))?
            .ok_or(DatabaseError::NotFound)?;
        row_to_record(&row)
    }

    async fn run_execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let mut conn = self.connection().await?;
        execute(&mut conn, sql, params).await
    }

    /// Delegates to [`run_fetch`](Self::run_fetch): Postgres has one pool
    /// with no read/write split, so this is behaviorally identical to
    /// `run_fetch` today, and delegating (rather than duplicating the bind +
    /// `fetch_all` + decode loop) keeps it that way by construction — a
    /// future change to parameter binding or row decoding can't drift between
    /// the two. This stays its own trait method — a distinct *contract* (a
    /// write statement that returns rows) — so the day this backend grows a
    /// read replica / reader-pool split, only this delegation needs to change
    /// to point at the write pool instead of `run_fetch`.
    async fn run_execute_returning(
        &self,
        sql: &str,
        params: &[serde_json::Value],
        json: &JsonColumns,
    ) -> Result<Vec<Record>, DatabaseError> {
        self.run_fetch(sql, params, json).await
    }

    async fn run_scalar_i64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let mut conn = self.connection().await?;
        fetch_scalar(&mut conn, sql, params).await
    }

    async fn run_scalar_f64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<f64, DatabaseError> {
        let mut conn = self.connection().await?;
        fetch_scalar(&mut conn, sql, params).await
    }

    /// One pooled connection for the whole transaction. Returning early on a
    /// failed statement drops the uncommitted [`sqlx::Transaction`], which
    /// rolls it back (sqlx issues the `ROLLBACK` when the connection returns
    /// to the pool).
    async fn run_transaction(&self, ops: &[TxOp<'_>]) -> Result<Vec<TxResult>, DatabaseError> {
        let mut tx = self.pool.begin().await.map_err(|e| sqlx_error(&e))?;
        let mut results = Vec::with_capacity(ops.len());
        for op in ops {
            let (sql, params) = op.sql_params();
            let result = match op {
                TxOp::Execute { .. } => TxResult::Execute(execute(&mut tx, sql, params).await?),
                TxOp::Returning { .. } => {
                    let rows = fetch_all(&mut tx, sql, params).await?;
                    TxResult::Returning(rows.iter().map(row_to_record).collect::<Result<_, _>>()?)
                }
            };
            results.push(result);
        }
        tx.commit().await.map_err(|e| sqlx_error(&e))?;
        Ok(results)
    }

    async fn dbx_table_exists(&self, table: &str) -> Result<bool, DatabaseError> {
        let (sql, params) = introspect::build_table_exists(table, Backend::Postgres);
        let mut conn = self.connection().await?;
        fetch_scalar(&mut conn, &sql, &params).await
    }
}

forward_database_service! {
    impl DatabaseService for PostgresDatabaseService {
        forward_to DbExec;

        ops {
            get: forward,
            list: forward,
            create: forward,
            create_many: forward,
            update: forward,
            delete: forward,
            count: forward,
            sum: forward,
            query_raw: forward,
            exec_raw: forward,
            delete_where: forward,
            delete_where_count: forward,
            take_where: forward,
            update_where: forward,
            update_where_count: forward,
            increment_field_where: forward,
            upsert: forward,
            aggregate: forward,
            batch: forward,
            insert_guarded: forward,
            update_guarded: forward,
            // The shared default is CREATE → add missing declared columns →
            // indexes → FK indexes, and it invalidates this backend's schema
            // cache on both the success and failure paths. The hand-written
            // version it replaces skipped the column adds entirely, so a table
            // that predated a schema revision never gained the new column.
            ensure_schema_table: forward,
            ensure_schema_tables: inherit,
            schema_table_exists: forward,
            schema_columns: forward,
            // `DbExec` has no schema-mutation primitives, and STRICT_SCHEMA is
            // per-backend state.
            schema_drop_table: custom,
            schema_add_column: custom,
            set_strict_schema: custom,
        }

        async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
            let result = self.schema_drop_table_async(name).await;
            self.schema_cache.invalidate(name);
            result
        }

        async fn schema_add_column(
            &self,
            table: &str,
            column: &Column,
        ) -> Result<(), DatabaseError> {
            let result = self.schema_add_column_async(table, column).await;
            self.schema_cache.invalidate(table);
            result
        }

        fn set_strict_schema(&self, enabled: bool) {
            self.strict_schema.store(enabled, Ordering::Relaxed);
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions: query building, type mapping, row conversion
// ---------------------------------------------------------------------------

/// Decode column `ordinal` as `Option<T>`, mapping a present value through
/// `to_json`.
///
/// Owns the `Ok(Some)`/`Ok(None)`/`Err` triple shared by every `row_to_record`
/// type arm. `Ok(None)` is a real SQL `NULL` and maps to JSON `null`. A
/// `try_get` `Err` means the decoder picked from the column's SQL type name
/// could not decode the stored value — i.e. this function's type mapping is
/// wrong for that column. That is a genuine backend bug, so it is returned as
/// an [`DatabaseError`], **never silently substituted with NULL**: a silent
/// NULL turns a wrong type mapping into a wrong *result* (e.g. an aggregate
/// count coming back empty) that no test or caller can see. Unlike SQLite —
/// whose reader keys off each value's *runtime* type and so cannot pick a
/// mismatched decoder — the Postgres reader picks by static SQL type name, so a
/// gap here is exactly the failure mode this hard error surfaces.
fn decode_col<'r, T>(
    row: &'r PgRow,
    ordinal: usize,
    col_name: &str,
    type_name: &str,
    to_json: impl FnOnce(T) -> serde_json::Value,
) -> Result<serde_json::Value, DatabaseError>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    match row.try_get::<Option<T>, _>(ordinal) {
        Ok(Some(v)) => Ok(to_json(v)),
        Ok(None) => Ok(serde_json::Value::Null),
        Err(e) => Err(DatabaseError::Internal(format!(
            "failed to decode column {col_name:?} (SQL type {type_name}): {e}"
        ))),
    }
}

/// Convert a PgRow to a Record, mapping column types to serde_json::Value.
///
/// The driver reports every column's type, so this needs no
/// [`JsonColumns`]: a `JSON`/`JSONB` column decodes structured and a text
/// column as text, which is what the executor's `json` names would say.
fn row_to_record(row: &PgRow) -> Result<Record, DatabaseError> {
    use sqlx::{Column as SqlxColumn, TypeInfo};

    let columns = row.columns();
    let mut data = HashMap::new();
    let mut id = String::new();

    for col in columns {
        let col_name = col.name().to_string();
        let type_name = col.type_info().name();
        let ordinal = col.ordinal();

        let value: serde_json::Value = match type_name {
            // A text column holds text, whatever it looks like. JSON lives in
            // `JSON`/`JSONB` columns, which the driver returns structured
            // (their arm below) — the declared type decides, as it does for
            // the SQLite family (see `codec`).
            "TEXT" | "VARCHAR" | "CHAR" | "NAME" | "BPCHAR" | "UNKNOWN" => decode_col(
                row,
                ordinal,
                &col_name,
                type_name,
                serde_json::Value::String,
            )?,
            "INT2" | "INT4" => decode_col(row, ordinal, &col_name, type_name, |n: i32| {
                serde_json::Value::Number(n.into())
            })?,
            "INT8" | "BIGINT" => decode_col(row, ordinal, &col_name, type_name, |n: i64| {
                serde_json::Value::Number(n.into())
            })?,
            "FLOAT4" => decode_col(row, ordinal, &col_name, type_name, |f: f32| {
                serde_json::Number::from_f64(f64::from(f))
                    .map_or(serde_json::Value::Null, serde_json::Value::Number)
            })?,
            "FLOAT8" | "DOUBLE PRECISION" => {
                decode_col(row, ordinal, &col_name, type_name, |f: f64| {
                    serde_json::Number::from_f64(f)
                        .map_or(serde_json::Value::Null, serde_json::Value::Number)
                })?
            }
            // NUMERIC needs its own decoder: sqlx's `f64` decode rejects the
            // Postgres `NUMERIC` wire type, so folding it into the FLOAT8 arm
            // above made every NUMERIC value fail to decode. Now that
            // `decode_col` hard-errors instead of silently NULLing, that would
            // be a loud failure for a perfectly ordinary result — e.g. `AVG(<int
            // column>)` and `SUM(<numeric column>)` both come back as NUMERIC.
            // Decode via `BigDecimal` (sqlx's `bigdecimal` feature) and convert
            // to the same `f64` JSON number every other numeric column produces.
            "NUMERIC" => decode_col(
                row,
                ordinal,
                &col_name,
                type_name,
                |d: sqlx::types::BigDecimal| {
                    d.to_string()
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                        .map_or(serde_json::Value::Null, serde_json::Value::Number)
                },
            )?,
            "BOOL" | "BOOLEAN" => {
                decode_col(row, ordinal, &col_name, type_name, serde_json::Value::Bool)?
            }
            "JSON" | "JSONB" => {
                decode_col(row, ordinal, &col_name, type_name, |v: serde_json::Value| v)?
            }
            "BYTEA" => decode_col(row, ordinal, &col_name, type_name, |b: Vec<u8>| {
                serde_json::Value::String(Base64::encode_string(&b))
            })?,
            "TIMESTAMPTZ" | "TIMESTAMP" => {
                // Try as a string first; a string-decode error here is not yet a
                // failure — Postgres returns these as a native type, so we fall
                // through to the chrono decoder, whose failure on a non-NULL
                // value is the one that hard-errors via `decode_col`.
                match row.try_get::<Option<String>, _>(ordinal) {
                    Ok(Some(s)) => serde_json::Value::String(s),
                    Ok(None) => serde_json::Value::Null,
                    Err(_) => decode_col(
                        row,
                        ordinal,
                        &col_name,
                        type_name,
                        |dt: chrono::DateTime<chrono::Utc>| {
                            serde_json::Value::String(dt.to_rfc3339())
                        },
                    )?,
                }
            }
            "UUID" => decode_col(row, ordinal, &col_name, type_name, |u: uuid::Uuid| {
                serde_json::Value::String(u.to_string())
            })?,
            // Fallback: try as string.
            _ => decode_col(
                row,
                ordinal,
                &col_name,
                type_name,
                serde_json::Value::String,
            )?,
        };

        if col_name == "id" {
            id = codec::record_id(&value);
        }

        data.insert(col_name, value);
    }

    Ok(Record { id, data })
}

/// Refuse connect options that turn the per-connection statement cache off
/// (see [`PostgresDatabaseService::from_pool`]). sqlx exposes the capacity
/// only through the options' URL form, which always carries it.
fn require_statement_cache(options: &PgConnectOptions) -> Result<(), DatabaseError> {
    let url = options.to_url_lossy();
    let disabled = url
        .query_pairs()
        .any(|(key, value)| key == "statement-cache-capacity" && value == "0");
    if disabled {
        return Err(DatabaseError::InvalidArgument(
            "wafer-block-postgres needs a statement cache: remove \
             statement-cache-capacity=0 from the connection options"
                .to_string(),
        ));
    }
    Ok(())
}

/// Run `sql` with `params` on `conn`, returning its rows.
async fn fetch_all(
    conn: &mut PgConnection,
    sql: &str,
    params: &[serde_json::Value],
) -> Result<Vec<PgRow>, DatabaseError> {
    let args = params::bind(conn, sql, params).await?;
    sqlx::query_with(sql, args)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| sqlx_error(&e))
}

/// Run `sql` with `params` on `conn`, returning the affected-row count.
async fn execute(
    conn: &mut PgConnection,
    sql: &str,
    params: &[serde_json::Value],
) -> Result<i64, DatabaseError> {
    let args = params::bind(conn, sql, params).await?;
    let done = sqlx::query_with(sql, args)
        .execute(&mut *conn)
        .await
        .map_err(|e| sqlx_error(&e))?;
    i64::try_from(done.rows_affected())
        .map_err(|_| DatabaseError::Internal("affected-row count out of range".into()))
}

/// Run `sql` with `params` on `conn`, returning the single scalar of its one
/// row.
async fn fetch_scalar<T>(
    conn: &mut PgConnection,
    sql: &str,
    params: &[serde_json::Value],
) -> Result<T, DatabaseError>
where
    T: for<'r> sqlx::Decode<'r, Postgres> + sqlx::Type<Postgres> + Send + Unpin,
{
    let args = params::bind(conn, sql, params).await?;
    sqlx::query_scalar_with(sql, args)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| sqlx_error(&e))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Without a statement cache every prepare would leave a named statement
    /// on the server connection, so a URL or pool that turns it off is refused
    /// up front — before any connection is attempted.
    #[tokio::test]
    async fn a_disabled_statement_cache_is_refused() {
        let err = PostgresDatabaseService::connect(
            "postgres://nobody:pw@127.0.0.1:1/nothing?statement-cache-capacity=0",
        )
        .await
        .err()
        .expect("refused");
        assert!(matches!(err, DatabaseError::InvalidArgument(_)), "{err:?}");

        let options: PgConnectOptions = "postgres://nobody:pw@127.0.0.1:1/nothing"
            .parse()
            .expect("options");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy_with(options.clone().statement_cache_capacity(0));
        assert!(matches!(
            PostgresDatabaseService::from_pool(pool),
            Err(DatabaseError::InvalidArgument(_))
        ));
        let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy_with(options);
        assert!(PostgresDatabaseService::from_pool(pool).is_ok());
    }

    /// Nothing listens on loopback port 1, so every connection is refused: a
    /// fault that says nothing about the request and may clear once the
    /// server is up. It is `Unavailable`, the code a block Init that hit it is
    /// retried on. (The pool keeps retrying the refused connect until its
    /// acquire timeout, shortened here; `connect` fails the same way after
    /// the default 30 s.)
    #[tokio::test]
    async fn an_unreachable_server_is_unavailable() {
        const NOWHERE: &str = "postgres://nobody:pw@127.0.0.1:1/nothing";
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(500))
            .connect_lazy(NOWHERE)
            .expect("a lazy pool connects on first use");
        let svc = PostgresDatabaseService::from_pool(pool).expect("a pool with a statement cache");
        let err = DbExec::get(&svc, "anything", "id")
            .await
            .expect_err("nothing to connect to");
        assert!(matches!(err, DatabaseError::Unavailable(_)), "{err:?}");
        assert_eq!(err.code(), wafer_block::ErrorCode::Unavailable);
    }

    #[test]
    fn test_sea_query_select_with_filters() {
        let opts = ListOptions {
            filters: vec![Filter {
                field: "name".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!("alice"),
            }],
            sort: vec![],
            limit: None,
            offset: 0,
            skip_count: false,
            filter_tree: None,
            columns: None,
        };
        let stmt = wafer_sql_utils::query::build_select("users", &opts, &["id"], Backend::Postgres)
            .expect("renders");
        let sql = stmt.sql;
        assert!(sql.contains("WHERE"));
        assert!(sql.contains("$1"));
        let params = sea_values_to_json(stmt.values);
        assert_eq!(params.len(), 1);
        assert_eq!(params[0], serde_json::json!("alice"));
    }

    #[test]
    fn test_sea_query_select_with_sort_and_pagination() {
        let opts = ListOptions {
            filters: vec![],
            sort: vec![
                SortField {
                    field: "created_at".to_string(),
                    desc: true,
                },
                SortField {
                    field: "name".to_string(),
                    desc: false,
                },
            ],
            limit: Some(10),
            offset: 20,
            skip_count: false,
            filter_tree: None,
            columns: None,
        };
        let stmt = wafer_sql_utils::query::build_select("items", &opts, &["id"], Backend::Postgres)
            .expect("renders");
        assert!(stmt.sql.contains("ORDER BY"));
        assert!(stmt.sql.contains("LIMIT"));
        assert!(stmt.sql.contains("OFFSET"));
    }

    #[test]
    fn test_sea_query_count_with_filters() {
        let filters = vec![
            Filter {
                field: "age".to_string(),
                operator: FilterOp::GreaterThan,
                value: serde_json::json!(18),
            },
            Filter {
                field: "active".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(true),
            },
        ];
        let stmt = wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Postgres);
        let sql = stmt.sql;
        assert!(sql.contains("COUNT(*)"));
        assert!(sql.contains("WHERE"));
        assert!(sql.contains("$1"));
        assert!(sql.contains("$2"));
        let params = sea_values_to_json(stmt.values);
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_sea_query_delete_where() {
        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::In,
            value: serde_json::json!(["active", "pending", "review"]),
        }];
        let stmt = wafer_sql_utils::query::build_delete_where("users", &filters, Backend::Postgres);
        let sql = stmt.sql;
        assert!(sql.contains("DELETE FROM"));
        assert!(sql.contains("IN"));
        let params = sea_values_to_json(stmt.values);
        assert_eq!(params.len(), 3);
        assert_eq!(params[0], serde_json::json!("active"));
        assert_eq!(params[1], serde_json::json!("pending"));
        assert_eq!(params[2], serde_json::json!("review"));
    }

    #[test]
    fn test_sea_query_update_where() {
        let data = vec![("status".to_string(), serde_json::json!("active"))];
        let filters = vec![Filter {
            field: "id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("123"),
        }];
        let stmt =
            wafer_sql_utils::query::build_update_where("users", &data, &filters, Backend::Postgres);
        let sql = stmt.sql;
        assert!(sql.contains("UPDATE"));
        assert!(sql.contains("SET"));
        assert!(sql.contains("WHERE"));
        assert!(sql.contains("$1"));
        assert!(sql.contains("$2"));
        let params = sea_values_to_json(stmt.values);
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_sea_query_sum() {
        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("active"),
        }];
        let stmt =
            wafer_sql_utils::aggregate::build_sum("orders", "amount", &filters, Backend::Postgres);
        let sql = stmt.sql;
        assert!(sql.contains("SUM"));
        assert!(sql.contains("COALESCE"));
        assert!(sql.contains("WHERE"));
        // 2 params: the COALESCE default (0) + the filter value
        let params = sea_values_to_json(stmt.values);
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_sea_query_is_null_filter() {
        let filters = vec![Filter {
            field: "deleted_at".to_string(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        }];
        let stmt = wafer_sql_utils::query::build_delete_where("users", &filters, Backend::Postgres);
        assert!(stmt.sql.contains("IS NULL"));
        let params = sea_values_to_json(stmt.values);
        assert!(params.is_empty());
    }

    #[test]
    fn test_sea_query_is_not_null_filter() {
        let filters = vec![Filter {
            field: "email".to_string(),
            operator: FilterOp::IsNotNull,
            value: serde_json::Value::Null,
        }];
        let stmt = wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Postgres);
        assert!(stmt.sql.contains("IS NOT NULL"));
        let params = sea_values_to_json(stmt.values);
        assert!(params.is_empty());
    }

    #[test]
    fn test_sea_query_like_filter() {
        let filters = vec![Filter {
            field: "name".to_string(),
            operator: FilterOp::Like,
            value: serde_json::json!("%alice%"),
        }];
        let stmt = wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Postgres);
        let sql = stmt.sql;
        assert!(sql.contains("LIKE"));
        assert!(sql.contains("$1"));
        let params = sea_values_to_json(stmt.values);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_sea_query_comparison_ops() {
        let filters = vec![
            Filter {
                field: "age".to_string(),
                operator: FilterOp::GreaterEqual,
                value: serde_json::json!(18),
            },
            Filter {
                field: "score".to_string(),
                operator: FilterOp::LessThan,
                value: serde_json::json!(100),
            },
            Filter {
                field: "rank".to_string(),
                operator: FilterOp::LessEqual,
                value: serde_json::json!(10),
            },
        ];
        let stmt = wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Postgres);
        let sql = stmt.sql;
        assert!(sql.contains("$1"));
        assert!(sql.contains("$2"));
        assert!(sql.contains("$3"));
        let params = sea_values_to_json(stmt.values);
        assert_eq!(params.len(), 3);
    }

    // pg type mapping for lazy column-add now lives in
    // wafer_sql_utils::ddl::column_type_for_value (tested there).

    // Filter/clause/order tests now covered by wafer-sql-utils::query::tests
    // Schema DDL tests now live in wafer-sql-utils::ddl::tests

    #[test]
    fn test_sea_query_delete_where_count_postgres() {
        // Confirm the DELETE SQL produced has the right shape (no RETURNING).
        // The affected-row count comes from execute()'s rows_affected(), not SQL.
        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("active"),
        }];
        let stmt = wafer_sql_utils::query::build_delete_where("users", &filters, Backend::Postgres);
        let sql = stmt.sql;
        assert!(sql.contains("DELETE FROM"));
        assert!(sql.contains("WHERE"));
        assert!(sql.contains("$1"));
        let params = sea_values_to_json(stmt.values);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_sea_query_delete_where_returning_postgres() {
        let filters = vec![Filter {
            field: "code".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("abc123"),
        }];
        let stmt = wafer_sql_utils::query::build_delete_where_returning(
            "codes",
            &filters,
            Backend::Postgres,
        );
        let sql = stmt.sql;
        assert!(sql.contains("DELETE FROM"));
        assert!(sql.contains("WHERE"));
        assert!(
            sql.contains("RETURNING"),
            "should contain RETURNING clause: {sql}"
        );
        let params = sea_values_to_json(stmt.values);
        assert_eq!(params.len(), 1);
    }
}
