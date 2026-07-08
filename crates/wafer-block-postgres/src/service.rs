use std::collections::HashMap;

use base64ct::{Base64, Encoding};
use sqlx::{postgres::PgRow, PgPool, Row};
use wafer_block::db::{Filter, ListOptions};
#[cfg(test)]
use wafer_block::db::{FilterOp, SortField};
use wafer_block_macro::wafer_async_trait;
use wafer_core::interfaces::database::{
    exec::DbExec,
    service::{
        AggregateSpec, Column, DatabaseError, DatabaseService, Record, RecordList, Table,
        UpsertSpec,
    },
};
use wafer_sql_utils::{ddl, introspect, Backend};
#[cfg(test)]
use wafer_sql_utils::{ident::sanitize_ident, value::sea_values_to_json};

/// PostgreSQL implementation of the DatabaseService.
///
/// Uses `sqlx` with connection pooling.
pub struct PostgresDatabaseService {
    pool: PgPool,
}

impl PostgresDatabaseService {
    /// Connect to a PostgreSQL database using a connection URL.
    pub async fn connect(url: &str) -> Result<Self, DatabaseError> {
        let pool = PgPool::connect(url)
            .await
            .map_err(|e| DatabaseError::Internal(format!("connect: {e}")))?;
        Ok(Self { pool })
    }

    /// Create a service from an existing connection pool.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    // -----------------------------------------------------------------
    // Schema DDL async helpers
    // -----------------------------------------------------------------

    async fn schema_ensure_table_async(&self, table: &Table) -> Result<(), DatabaseError> {
        let create_stmt = ddl::build_create_table(table, Backend::Postgres).map_err(|e| {
            DatabaseError::Internal(format!("build create table {}: {}", table.name, e))
        })?;
        sqlx::query(&create_stmt.sql)
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(format!("create table {}: {}", table.name, e)))?;

        // Ensure indexes
        for idx in &table.indexes {
            let idx_stmt = ddl::build_create_index(&table.name, idx, Backend::Postgres)
                .map_err(|e| DatabaseError::Internal(format!("build create index: {e}")))?;
            sqlx::query(&idx_stmt.sql)
                .execute(&self.pool)
                .await
                .map_err(|e| DatabaseError::Internal(format!("create index: {e}")))?;
        }

        // Create indexes for columns with foreign keys
        let fk_stmts = ddl::build_fk_indexes(table, Backend::Postgres)
            .map_err(|e| DatabaseError::Internal(format!("build FK indexes: {e}")))?;
        for stmt in fk_stmts {
            sqlx::query(&stmt.sql)
                .execute(&self.pool)
                .await
                .map_err(|e| DatabaseError::Internal(format!("create FK index: {e}")))?;
        }

        Ok(())
    }

    async fn schema_drop_table_async(&self, name: &str) -> Result<(), DatabaseError> {
        let stmt = ddl::build_drop_table(name, Backend::Postgres);
        sqlx::query(&stmt.sql)
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(format!("drop_table: {e}")))?;
        Ok(())
    }

    async fn schema_add_column_async(
        &self,
        table: &str,
        column: &Column,
    ) -> Result<(), DatabaseError> {
        let stmt = ddl::build_add_column(table, column, Backend::Postgres);
        sqlx::query(&stmt.sql)
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(format!("add_column: {e}")))?;
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

    async fn run_fetch(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        let mut q = sqlx::query(sql);
        for p in params {
            q = bind_json_value_query(q, p);
        }
        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let mut records = Vec::with_capacity(rows.len());
        for row in &rows {
            records.push(row_to_record(row)?);
        }
        Ok(records)
    }

    async fn run_fetch_one(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Record, DatabaseError> {
        let mut q = sqlx::query(sql);
        for p in params {
            q = bind_json_value_query(q, p);
        }
        let row = q.fetch_one(&self.pool).await.map_err(|e| match e {
            sqlx::Error::RowNotFound => DatabaseError::NotFound,
            _ => DatabaseError::Internal(e.to_string()),
        })?;
        row_to_record(&row)
    }

    async fn run_execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let mut q = sqlx::query(sql);
        for p in params {
            q = bind_json_value_query(q, p);
        }
        let result = q
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        Ok(result.rows_affected() as i64)
    }

    async fn run_scalar_i64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let mut q = sqlx::query_scalar::<_, i64>(sql);
        for p in params {
            q = bind_json_value(q, p);
        }
        q.fetch_one(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    async fn run_scalar_f64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<f64, DatabaseError> {
        let mut q = sqlx::query_scalar::<_, f64>(sql);
        for p in params {
            q = bind_json_value(q, p);
        }
        q.fetch_one(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    async fn dbx_table_exists(&self, table: &str) -> Result<bool, DatabaseError> {
        let (sql, params) = introspect::build_table_exists(table, Backend::Postgres);
        let mut q = sqlx::query_scalar::<_, bool>(&sql);
        for p in &params {
            q = bind_json_value(q, p);
        }
        q.fetch_one(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(format!("table_exists: {e}")))
    }
}

#[wafer_async_trait]
impl DatabaseService for PostgresDatabaseService {
    async fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError> {
        DbExec::get(self, collection, id).await
    }

    async fn list(
        &self,
        collection: &str,
        opts: &ListOptions,
    ) -> Result<RecordList, DatabaseError> {
        DbExec::list(self, collection, opts).await
    }

    async fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        DbExec::create(self, collection, data).await
    }

    async fn update(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        DbExec::update(self, collection, id, data).await
    }

    async fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError> {
        DbExec::delete(self, collection, id).await
    }

    async fn count(&self, collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError> {
        DbExec::count(self, collection, filters).await
    }

    async fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        DbExec::sum(self, collection, field, filters).await
    }

    async fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        DbExec::query_raw(self, query, args).await
    }

    async fn exec_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        DbExec::exec_raw(self, query, args).await
    }

    // --- Schema management ---

    async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
        self.schema_ensure_table_async(table).await
    }

    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        DbExec::schema_table_exists(self, name).await
    }

    async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
        self.schema_drop_table_async(name).await
    }

    async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError> {
        self.schema_add_column_async(table, column).await
    }

    async fn delete_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<(), DatabaseError> {
        DbExec::delete_where(self, collection, filters).await
    }

    async fn delete_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        DbExec::delete_where_count(self, collection, filters).await
    }

    async fn take_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<Vec<Record>, DatabaseError> {
        DbExec::take_where(self, collection, filters).await
    }

    async fn update_where(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        DbExec::update_where(self, collection, filters, data).await
    }

    async fn increment_field_where(
        &self,
        collection: &str,
        col: &str,
        delta: i64,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        DbExec::increment_field_where(self, collection, col, delta, filters).await
    }

    async fn upsert(&self, collection: &str, spec: UpsertSpec) -> Result<i64, DatabaseError> {
        DbExec::upsert(self, collection, spec).await
    }

    async fn aggregate(
        &self,
        collection: &str,
        spec: AggregateSpec,
    ) -> Result<Vec<Record>, DatabaseError> {
        DbExec::aggregate(self, collection, spec).await
    }
}

// ---------------------------------------------------------------------------
// Free functions: query building, type mapping, row conversion
// ---------------------------------------------------------------------------

/// Decode column `ordinal` as `Option<T>`, mapping a present value through
/// `to_json`.
///
/// Owns the `Ok(Some)`/`Ok(None)`/`Err` triple shared by every `row_to_record`
/// type arm: a `try_get` `Err` means the column's stored type didn't match the
/// decoder picked from the SQL type name — a genuine data/schema problem, not
/// a SQL NULL. We keep the NULL fallback (so a single bad column doesn't sink
/// the whole row) but log it via `tracing::warn!` so it's observable,
/// mirroring the SQLite read path's skip-and-warn. `Ok(None)` is a real NULL
/// and stays silent.
fn decode_col<'r, T>(
    row: &'r PgRow,
    ordinal: usize,
    col_name: &str,
    type_name: &str,
    to_json: impl FnOnce(T) -> serde_json::Value,
) -> serde_json::Value
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    match row.try_get::<Option<T>, _>(ordinal) {
        Ok(Some(v)) => to_json(v),
        Ok(None) => serde_json::Value::Null,
        Err(e) => {
            tracing::warn!(column = %col_name, sql_type = %type_name, error = %e, "failed to decode column; substituting NULL");
            serde_json::Value::Null
        }
    }
}

/// Convert a PgRow to a Record, mapping column types to serde_json::Value.
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
            "TEXT" | "VARCHAR" | "CHAR" | "NAME" | "BPCHAR" | "UNKNOWN" => decode_col(
                row,
                ordinal,
                &col_name,
                type_name,
                serde_json::Value::String,
            ),
            "INT2" | "INT4" => decode_col(row, ordinal, &col_name, type_name, |n: i32| {
                serde_json::Value::Number(n.into())
            }),
            "INT8" | "BIGINT" => decode_col(row, ordinal, &col_name, type_name, |n: i64| {
                serde_json::Value::Number(n.into())
            }),
            "FLOAT4" => decode_col(row, ordinal, &col_name, type_name, |f: f32| {
                serde_json::Number::from_f64(f64::from(f))
                    .map_or(serde_json::Value::Null, serde_json::Value::Number)
            }),
            "FLOAT8" | "DOUBLE PRECISION" | "NUMERIC" => {
                decode_col(row, ordinal, &col_name, type_name, |f: f64| {
                    serde_json::Number::from_f64(f)
                        .map_or(serde_json::Value::Null, serde_json::Value::Number)
                })
            }
            "BOOL" | "BOOLEAN" => {
                decode_col(row, ordinal, &col_name, type_name, serde_json::Value::Bool)
            }
            "JSON" | "JSONB" => {
                decode_col(row, ordinal, &col_name, type_name, |v: serde_json::Value| v)
            }
            "BYTEA" => decode_col(row, ordinal, &col_name, type_name, |b: Vec<u8>| {
                serde_json::Value::String(Base64::encode_string(&b))
            }),
            "TIMESTAMPTZ" | "TIMESTAMP" => {
                // Try as a string first; a string-decode error here is not yet
                // a failure — Postgres returns these as a native type, so we
                // fall through to the chrono decoder, whose failure on a
                // non-NULL value is the one that warns.
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
                    ),
                }
            }
            "UUID" => decode_col(row, ordinal, &col_name, type_name, |u: uuid::Uuid| {
                serde_json::Value::String(u.to_string())
            }),
            // Fallback: try as string
            _ => decode_col(
                row,
                ordinal,
                &col_name,
                type_name,
                serde_json::Value::String,
            ),
        };

        if col_name == "id" {
            id = match &value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => String::new(),
            };
        }

        data.insert(col_name, value);
    }

    Ok(Record { id, data })
}

/// `sqlx::query` and `sqlx::query_scalar` builders expose an identical `bind`
/// method but share no trait, so one definition generates both bind helpers.
macro_rules! generate_bind {
    ($(#[$meta:meta])* fn $name:ident<$lt:lifetime $(, $gen:ident)?>($qty:ty)) => {
        $(#[$meta])*
        fn $name<$lt $(, $gen)?>(q: $qty, v: &$lt serde_json::Value) -> $qty {
            match v {
                serde_json::Value::Null => q.bind(None::<String>),
                serde_json::Value::Bool(b) => q.bind(*b),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        q.bind(i)
                    } else if let Some(f) = n.as_f64() {
                        q.bind(f)
                    } else {
                        q.bind(n.to_string())
                    }
                }
                serde_json::Value::String(s) => q.bind(s.as_str()),
                serde_json::Value::Array(_) | serde_json::Value::Object(_) => q.bind(v.clone()),
            }
        }
    };
}

generate_bind!(
    /// Bind a serde_json::Value to a `sqlx::query_scalar` query.
    fn bind_json_value<'q, O>(
        sqlx::query::QueryScalar<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>
    )
);

generate_bind!(
    /// Bind a serde_json::Value to a `sqlx::query` (non-scalar).
    fn bind_json_value_query<'q>(sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>)
);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sea_query_select_with_filters() {
        let opts = ListOptions {
            filters: vec![Filter {
                field: "name".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!("alice"),
            }],
            sort: vec![],
            limit: 0,
            offset: 0,
            skip_count: false,
            filter_tree: None,
            columns: None,
        };
        let stmt = wafer_sql_utils::query::build_select("users", &opts, Backend::Postgres);
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
            limit: 10,
            offset: 20,
            skip_count: false,
            filter_tree: None,
            columns: None,
        };
        let stmt = wafer_sql_utils::query::build_select("items", &opts, Backend::Postgres);
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

    #[test]
    fn test_sanitize_ident() {
        assert_eq!(sanitize_ident("users"), "users");
        assert_eq!(sanitize_ident("my_table"), "my_table");
        assert_eq!(sanitize_ident("table123"), "table123");
        assert_eq!(sanitize_ident("drop table;--"), "droptable");
        assert_eq!(
            sanitize_ident("Robert'); DROP TABLE users;--"),
            "RobertDROPTABLEusers"
        );
    }

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
