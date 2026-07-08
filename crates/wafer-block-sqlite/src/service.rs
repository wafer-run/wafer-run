use std::{collections::HashMap, sync::Mutex};

use base64ct::{Base64, Encoding};
use rusqlite::{types::Value as SqlValue, Connection, Row};
use wafer_block::db::{Filter, ListOptions};
use wafer_block_macro::wafer_async_trait;
#[cfg(test)]
use wafer_core::interfaces::database::service::{pk, DataType};
use wafer_core::interfaces::database::{
    exec::DbExec,
    service::{Column, DatabaseError, DatabaseService, Record, RecordList, Table, UpsertSpec},
};
use wafer_sql_utils::{ddl, introspect, Backend};

/// SQLite implementation of the DatabaseService.
pub struct SQLiteDatabaseService {
    db: Mutex<Connection>,
}

impl SQLiteDatabaseService {
    /// Wrap an open `rusqlite::Connection`, enabling WAL journaling,
    /// foreign-key enforcement, and a 5s busy timeout. PRAGMA failures
    /// are logged but non-fatal so callers always get a usable service.
    pub(crate) fn new(db: Connection) -> Self {
        // Enable WAL mode and foreign keys
        if let Err(e) = db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        ) {
            tracing::warn!(error = %e, "failed to set SQLite PRAGMAs — performance and safety may be degraded");
        }
        Self { db: Mutex::new(db) }
    }

    /// Open a SQLite database file at `path` (creating it if absent) and
    /// return a configured service. Used by `solobase-native` to back the
    /// `wafer-run/sqlite` block with an on-disk DB.
    pub fn open(path: &str) -> Result<Self, DatabaseError> {
        let conn = Connection::open(path)
            .map_err(|e| DatabaseError::Internal(format!("open database: {e}")))?;
        Ok(Self::new(conn))
    }

    /// Open an in-memory SQLite database for tests and ephemeral
    /// workloads. The connection lives for the lifetime of the returned
    /// service and is dropped with it.
    pub fn open_in_memory() -> Result<Self, DatabaseError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| DatabaseError::Internal(format!("open in-memory database: {e}")))?;
        Ok(Self::new(conn))
    }

    fn row_to_record(row: &Row) -> rusqlite::Result<Record> {
        let column_count = row.as_ref().column_count();
        let mut data = HashMap::new();
        let mut id = String::new();

        for i in 0..column_count {
            let col_name = row.as_ref().column_name(i).unwrap_or("").to_string();
            let value = match row.get_ref(i) {
                Ok(rusqlite::types::ValueRef::Null) => serde_json::Value::Null,
                Ok(rusqlite::types::ValueRef::Integer(n)) => serde_json::Value::Number(n.into()),
                Ok(rusqlite::types::ValueRef::Real(f)) => serde_json::Number::from_f64(f)
                    .map_or(serde_json::Value::Null, serde_json::Value::Number),
                Ok(rusqlite::types::ValueRef::Text(s)) => {
                    let text = String::from_utf8_lossy(s).to_string();
                    // Try to parse as JSON if it looks like JSON
                    if (text.starts_with('{') && text.ends_with('}'))
                        || (text.starts_with('[') && text.ends_with(']'))
                    {
                        serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
                    } else {
                        serde_json::Value::String(text)
                    }
                }
                Ok(rusqlite::types::ValueRef::Blob(b)) => {
                    serde_json::Value::String(Base64::encode_string(b))
                }
                Err(_) => serde_json::Value::Null,
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
}

fn json_to_sql_value(v: &serde_json::Value) -> SqlValue {
    match v {
        serde_json::Value::Null => SqlValue::Null,
        serde_json::Value::Bool(b) => SqlValue::Integer(if *b { 1 } else { 0 }),
        serde_json::Value::Number(n) => n.as_i64().map_or_else(
            || {
                n.as_f64()
                    .map_or_else(|| SqlValue::Text(n.to_string()), SqlValue::Real)
            },
            SqlValue::Integer,
        ),
        serde_json::Value::String(s) => SqlValue::Text(s.clone()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => SqlValue::Text(v.to_string()),
    }
}

/// Get list of column names for an existing table.
///
/// Synchronous sibling of the shared `DbExec::get_columns` default, for
/// callers that already hold the connection lock (`ensure_schema_table`).
/// Propagates real DB errors as [`DatabaseError`]; a row that fails to decode
/// is a real error too (the introspection shape is fixed), so we surface it
/// rather than silently dropping the column from the set.
fn table_columns(db: &Connection, table: &str) -> Result<Vec<String>, DatabaseError> {
    let (sql, params) = introspect::build_list_columns(table, Backend::Sqlite);
    let bound: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
    let bound_refs: Vec<&dyn rusqlite::types::ToSql> = bound
        .iter()
        .map(|v| v as &dyn rusqlite::types::ToSql)
        .collect();
    let mut stmt = db
        .prepare(&sql)
        .map_err(|e| DatabaseError::Internal(format!("prepare list_columns {table}: {e}")))?;
    let mut cols = Vec::new();
    let rows = stmt
        .query_map(bound_refs.as_slice(), |row| row.get::<_, String>(0))
        .map_err(|e| DatabaseError::Internal(format!("query list_columns {table}: {e}")))?;
    for row in rows {
        let name = row.map_err(|e| {
            DatabaseError::Internal(format!("read list_columns row for {table}: {e}"))
        })?;
        cols.push(name.to_lowercase());
    }
    Ok(cols)
}

/// Check if the table's `id` column is INTEGER PRIMARY KEY (autoincrement).
fn has_integer_pk(db: &Connection, table: &str) -> bool {
    let Ok((sql, _)) = introspect::build_table_info(table, Backend::Sqlite) else {
        return false;
    };
    let Ok(mut stmt) = db.prepare(&sql) else {
        return false;
    };
    let result = stmt.query_map([], |row| {
        let name: String = row.get(1)?;
        let col_type: String = row.get(2)?;
        let pk: i32 = row.get(5)?;
        Ok((name, col_type, pk))
    });
    if let Ok(rows) = result {
        for r in rows.flatten() {
            if r.0.to_lowercase() == "id" && r.2 > 0 && r.1.to_uppercase().contains("INT") {
                return true;
            }
        }
    }
    false
}

#[wafer_async_trait]
impl DbExec for SQLiteDatabaseService {
    const BACKEND: Backend = Backend::Sqlite;

    #[expect(
        clippy::significant_drop_tightening,
        reason = "guard must span prepare→query_map→collect; the prepared statement and MappedRows iterator both borrow the guard, so it cannot drop until rows is collected. Scope is already minimized to the inner block whose tail is the owned Vec."
    )]
    async fn run_fetch(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        let sql_params: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
        let query_params: Vec<&dyn rusqlite::types::ToSql> = sql_params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        let records: Vec<Record> = {
            let db = self
                .db
                .lock()
                .map_err(|e| DatabaseError::Internal(e.to_string()))?;
            let mut prepared = db
                .prepare(sql)
                .map_err(|e| DatabaseError::Internal(e.to_string()))?;
            // Bind to a `let` (rather than letting this be the block's tail
            // expression) so the `MappedRows` iterator temporary — which borrows
            // `prepared`/`db` — is dropped at the `;` before the guard goes out
            // of scope, while still keeping the lock held only for this block.
            let rows: Vec<Record> = prepared
                .query_map(query_params.as_slice(), Self::row_to_record)
                .map_err(|e| DatabaseError::Internal(e.to_string()))?
                .filter_map(|r| match r {
                    Ok(record) => Some(record),
                    Err(e) => {
                        tracing::warn!(error = %e, "skipping row due to deserialization error");
                        None
                    }
                })
                .collect();
            rows
        };
        Ok(records)
    }

    async fn run_fetch_one(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Record, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let sql_params: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
        let query_params: Vec<&dyn rusqlite::types::ToSql> = sql_params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        db.query_row(sql, query_params.as_slice(), Self::row_to_record)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => DatabaseError::NotFound,
                _ => DatabaseError::Internal(e.to_string()),
            })
    }

    async fn run_execute(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let sql_params: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
        let query_params: Vec<&dyn rusqlite::types::ToSql> = sql_params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = db
            .execute(sql, query_params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        drop(db);
        Ok(rows as i64)
    }

    async fn run_scalar_i64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let sql_params: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
        let query_params: Vec<&dyn rusqlite::types::ToSql> = sql_params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        db.query_row(sql, query_params.as_slice(), |row| row.get(0))
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    async fn run_scalar_f64(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<f64, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let sql_params: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
        let query_params: Vec<&dyn rusqlite::types::ToSql> = sql_params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        db.query_row(sql, query_params.as_slice(), |row| row.get(0))
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    async fn dbx_table_exists(&self, table: &str) -> Result<bool, DatabaseError> {
        let (sql, params) = introspect::build_table_exists(table, Backend::Sqlite);
        Ok(self.run_scalar_i64(&sql, &params).await? > 0)
    }

    /// Lock-spanning insert: `last_insert_rowid()` is only meaningful while no
    /// other insert can run on the connection, so the guard covers both calls.
    async fn run_insert(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Option<i64>, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let sql_params: Vec<SqlValue> = params.iter().map(json_to_sql_value).collect();
        let query_params: Vec<&dyn rusqlite::types::ToSql> = sql_params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        db.execute(sql, query_params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let rowid = db.last_insert_rowid();
        drop(db);
        Ok(Some(rowid))
    }

    /// Tables with `INTEGER PRIMARY KEY` autoincrement generate their own id;
    /// `create` must not synthesize a UUID for them.
    async fn table_autogenerates_id(&self, table: &str) -> bool {
        self.db
            .lock()
            .map_or(false, |db| has_integer_pk(&db, table))
    }
}

#[wafer_async_trait]
impl DatabaseService for SQLiteDatabaseService {
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

    // --- Schema management ---

    async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let create_stmt = ddl::build_create_table(table, Backend::Sqlite).map_err(|e| {
            DatabaseError::Internal(format!("build create table {}: {}", table.name, e))
        })?;
        db.execute_batch(&create_stmt.sql)
            .map_err(|e| DatabaseError::Internal(format!("create table {}: {}", table.name, e)))?;

        // Add any missing columns. The table was just created above, so a
        // failure to read its columns is a real error, not "no columns" —
        // propagate it (matches `table_columns`' fail-loud contract). The
        // individual `ADD COLUMN` adds stay best-effort/warn since a duplicate
        // column is a benign re-run.
        let existing = table_columns(&db, &table.name)?;
        for col in &table.columns {
            if !existing.contains(&col.name.to_lowercase()) {
                let alter = ddl::build_add_column(&table.name, col, Backend::Sqlite);
                if let Err(e) = db.execute_batch(&alter.sql) {
                    tracing::warn!(table = %table.name, column = %col.name, error = %e, "failed to add column");
                }
            }
        }

        // Ensure indexes
        for idx in &table.indexes {
            let idx_stmt = ddl::build_create_index(&table.name, idx, Backend::Sqlite)
                .map_err(|e| DatabaseError::Internal(format!("build create index: {e}")))?;
            db.execute_batch(&idx_stmt.sql)
                .map_err(|e| DatabaseError::Internal(format!("create index: {e}")))?;
        }

        // Create indexes for columns with foreign keys
        let fk_stmts = ddl::build_fk_indexes(table, Backend::Sqlite)
            .map_err(|e| DatabaseError::Internal(format!("build FK indexes: {e}")))?;
        for stmt in fk_stmts {
            db.execute_batch(&stmt.sql)
                .map_err(|e| DatabaseError::Internal(format!("create FK index: {e}")))?;
        }
        drop(db);

        Ok(())
    }

    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        DbExec::schema_table_exists(self, name).await
    }

    async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
        let stmt = ddl::build_drop_table(name, Backend::Sqlite);
        self.run_execute(&stmt.sql, &[]).await?;
        Ok(())
    }

    async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError> {
        let stmt = ddl::build_add_column(table, column, Backend::Sqlite);
        self.run_execute(&stmt.sql, &[]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use wafer_block::db::{Filter, FilterOp, FilterTree, ListOptions, SortField};
    use wafer_sql_utils::value::sea_values_to_json;

    use super::*;

    // -----------------------------------------------------------------------
    // json_to_sql_value type conversion tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_json_to_sql_null() {
        assert_eq!(json_to_sql_value(&serde_json::Value::Null), SqlValue::Null);
    }

    #[test]
    fn test_json_to_sql_bool() {
        assert_eq!(
            json_to_sql_value(&serde_json::json!(true)),
            SqlValue::Integer(1)
        );
        assert_eq!(
            json_to_sql_value(&serde_json::json!(false)),
            SqlValue::Integer(0)
        );
    }

    #[test]
    fn test_json_to_sql_integer() {
        assert_eq!(
            json_to_sql_value(&serde_json::json!(42)),
            SqlValue::Integer(42)
        );
        assert_eq!(
            json_to_sql_value(&serde_json::json!(-7)),
            SqlValue::Integer(-7)
        );
    }

    #[test]
    fn test_json_to_sql_float() {
        assert_eq!(
            json_to_sql_value(&serde_json::json!(2.5)),
            SqlValue::Real(2.5)
        );
    }

    #[test]
    fn test_json_to_sql_string() {
        assert_eq!(
            json_to_sql_value(&serde_json::json!("hello")),
            SqlValue::Text("hello".to_string())
        );
    }

    #[test]
    fn test_json_to_sql_array() {
        let v = serde_json::json!([1, 2, 3]);
        match json_to_sql_value(&v) {
            SqlValue::Text(s) => assert_eq!(s, "[1,2,3]"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn test_json_to_sql_object() {
        let v = serde_json::json!({"key": "val"});
        match json_to_sql_value(&v) {
            SqlValue::Text(s) => {
                let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
                assert_eq!(parsed, serde_json::json!({"key": "val"}));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Sea-query builder integration tests (SQLite dialect)
    // -----------------------------------------------------------------------

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
        let stmt = wafer_sql_utils::query::build_select("users", &opts, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains("WHERE"));
        // SQLite uses ? placeholders, not $N
        assert!(sql.contains("?"), "SQLite should use ? placeholders");
        assert!(!sql.contains("$1"), "SQLite should not use $N placeholders");
        let params = sea_values_to_json(stmt.values)
            .iter()
            .map(json_to_sql_value)
            .collect::<Vec<_>>();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0], SqlValue::Text("alice".to_string()));
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
        let stmt = wafer_sql_utils::query::build_select("items", &opts, Backend::Sqlite);
        assert!(stmt.sql.contains("ORDER BY"));
        assert!(stmt.sql.contains("LIMIT"));
        assert!(stmt.sql.contains("OFFSET"));
    }

    #[test]
    fn test_sea_query_count_with_filters() {
        let filters = vec![Filter {
            field: "active".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(true),
        }];
        let stmt = wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains("COUNT(*)"));
        assert!(sql.contains("WHERE"));
        assert_eq!(sea_values_to_json(stmt.values).len(), 1);
    }

    #[test]
    fn test_sea_query_sum() {
        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("active"),
        }];
        let stmt =
            wafer_sql_utils::aggregate::build_sum("orders", "amount", &filters, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains("SUM"));
        assert!(sql.contains("COALESCE"));
        assert!(sql.contains("WHERE"));
        assert!(!sea_values_to_json(stmt.values).is_empty());
    }

    #[test]
    fn test_sea_query_delete_where() {
        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::In,
            value: serde_json::json!(["active", "pending"]),
        }];
        let stmt = wafer_sql_utils::query::build_delete_where("users", &filters, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains("DELETE FROM"));
        assert!(sql.contains("IN"));
        let params = sea_values_to_json(stmt.values)
            .iter()
            .map(json_to_sql_value)
            .collect::<Vec<_>>();
        assert_eq!(params.len(), 2);
        assert_eq!(params[0], SqlValue::Text("active".to_string()));
        assert_eq!(params[1], SqlValue::Text("pending".to_string()));
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
            wafer_sql_utils::query::build_update_where("users", &data, &filters, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains("UPDATE"));
        assert!(sql.contains("SET"));
        assert!(sql.contains("WHERE"));
        assert_eq!(sea_values_to_json(stmt.values).len(), 2);
    }

    #[test]
    fn test_sea_query_is_null_filter() {
        let filters = vec![Filter {
            field: "deleted_at".to_string(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        }];
        let stmt = wafer_sql_utils::query::build_delete_where("users", &filters, Backend::Sqlite);
        assert!(stmt.sql.contains("IS NULL"));
        assert!(sea_values_to_json(stmt.values).is_empty());
    }

    #[test]
    fn test_sea_query_is_not_null_filter() {
        let filters = vec![Filter {
            field: "email".to_string(),
            operator: FilterOp::IsNotNull,
            value: serde_json::Value::Null,
        }];
        let stmt = wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Sqlite);
        assert!(stmt.sql.contains("IS NOT NULL"));
        assert!(sea_values_to_json(stmt.values).is_empty());
    }

    #[test]
    fn test_sea_query_like_filter() {
        let filters = vec![Filter {
            field: "name".to_string(),
            operator: FilterOp::Like,
            value: serde_json::json!("%alice%"),
        }];
        let stmt = wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Sqlite);
        assert!(stmt.sql.contains("LIKE"));
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
        ];
        let stmt = wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains(">="));
        assert!(sql.contains("<"));
        assert_eq!(sea_values_to_json(stmt.values).len(), 2);
    }

    // -----------------------------------------------------------------------
    // Integration tests — delete_where_count + take_where (in-memory SQLite)
    // -----------------------------------------------------------------------

    fn make_test_svc() -> SQLiteDatabaseService {
        SQLiteDatabaseService::open_in_memory().unwrap()
    }

    async fn seed_rows(
        svc: &SQLiteDatabaseService,
        collection: &str,
        rows: Vec<serde_json::Value>,
    ) {
        // Declare a TEXT-everything schema from the union of row keys, then
        // create it via `ensure_schema_table` before inserting. The runtime
        // no longer auto-creates tables on first insert — production callers
        // run explicit migrations at `Init`, and the test fixture mirrors
        // that contract.
        let mut keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for row in &rows {
            if let serde_json::Value::Object(map) = row {
                for k in map.keys() {
                    if k != "id" && k != "created_at" && k != "updated_at" {
                        keys.insert(k.clone());
                    }
                }
            }
        }
        let mut columns = vec![pk("id")];
        for k in &keys {
            columns.push(Column::new(k, DataType::Text).null());
        }
        columns.push(Column::new("created_at", DataType::Text).null());
        columns.push(Column::new("updated_at", DataType::Text).null());
        let table = Table {
            name: collection.to_string(),
            columns,
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        };
        svc.ensure_schema_table(&table).await.unwrap();

        for row in rows {
            let mut data = std::collections::HashMap::new();
            if let serde_json::Value::Object(map) = row {
                for (k, v) in map {
                    data.insert(k, v);
                }
            }
            DatabaseService::create(svc, collection, data)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn delete_where_count_returns_affected_row_count() {
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "items",
            vec![
                serde_json::json!({"name": "alpha", "status": "active"}),
                serde_json::json!({"name": "beta", "status": "active"}),
                serde_json::json!({"name": "gamma", "status": "inactive"}),
            ],
        )
        .await;

        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("active"),
        }];

        let count = DatabaseService::delete_where_count(&svc, "items", &filters)
            .await
            .unwrap();
        assert_eq!(count, 2, "should have deleted exactly 2 active rows");

        // Remaining row is the inactive one
        let remaining = DatabaseService::count(&svc, "items", &[]).await.unwrap();
        assert_eq!(remaining, 1);
    }

    #[tokio::test]
    async fn delete_where_count_returns_zero_when_no_match() {
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "items",
            vec![serde_json::json!({"name": "alpha", "status": "active"})],
        )
        .await;

        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("nonexistent"),
        }];

        let count = DatabaseService::delete_where_count(&svc, "items", &filters)
            .await
            .unwrap();
        assert_eq!(count, 0);

        // Row still exists
        let remaining = DatabaseService::count(&svc, "items", &[]).await.unwrap();
        assert_eq!(remaining, 1);
    }

    #[tokio::test]
    async fn delete_where_count_on_missing_table_returns_zero() {
        let svc = make_test_svc();
        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("active"),
        }];
        let count = DatabaseService::delete_where_count(&svc, "no_such_table", &filters)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn take_where_returns_deleted_rows() {
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "codes",
            vec![
                serde_json::json!({"code": "abc123", "used": false}),
                serde_json::json!({"code": "xyz789", "used": false}),
                serde_json::json!({"code": "def456", "used": true}),
            ],
        )
        .await;

        let filters = vec![Filter {
            field: "used".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(false),
        }];

        let taken = DatabaseService::take_where(&svc, "codes", &filters)
            .await
            .unwrap();
        assert_eq!(taken.len(), 2, "should have taken 2 unused codes");

        // Verify the rows are actually deleted
        let remaining = DatabaseService::count(&svc, "codes", &[]).await.unwrap();
        assert_eq!(remaining, 1, "only 1 used code should remain");
    }

    #[tokio::test]
    async fn take_where_returns_empty_when_no_match() {
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "codes",
            vec![serde_json::json!({"code": "abc123", "used": true})],
        )
        .await;

        let filters = vec![Filter {
            field: "used".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(false),
        }];

        let taken = DatabaseService::take_where(&svc, "codes", &filters)
            .await
            .unwrap();
        assert!(taken.is_empty());

        // Original row still present
        let remaining = DatabaseService::count(&svc, "codes", &[]).await.unwrap();
        assert_eq!(remaining, 1);
    }

    #[tokio::test]
    async fn take_where_on_missing_table_returns_empty() {
        let svc = make_test_svc();
        let filters = vec![Filter {
            field: "code".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("abc"),
        }];
        let taken = DatabaseService::take_where(&svc, "no_such_table", &filters)
            .await
            .unwrap();
        assert!(taken.is_empty());
    }

    #[tokio::test]
    async fn increment_field_where_atomically_bumps_matching_rows() {
        let svc = make_test_svc();
        // access_count needs to be INTEGER for arithmetic; the TEXT-everything
        // helper seeds it as TEXT, so build the schema manually.
        let table = Table {
            name: "shares".into(),
            columns: vec![
                pk("id"),
                Column::new("access_count", DataType::Int).null(),
                Column::new("created_at", DataType::Text).null(),
                Column::new("updated_at", DataType::Text).null(),
            ],
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        };
        svc.ensure_schema_table(&table).await.unwrap();
        for id in ["a", "b", "c"] {
            let mut row = std::collections::HashMap::new();
            row.insert("id".into(), serde_json::json!(id));
            row.insert("access_count".into(), serde_json::json!(0));
            DatabaseService::create(&svc, "shares", row).await.unwrap();
        }

        // CAS-style bump on a single id with a max-cap predicate (the share.rs
        // pattern this op is built for).
        let filters = vec![
            Filter {
                field: "id".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!("a"),
            },
            Filter {
                field: "access_count".into(),
                operator: FilterOp::LessThan,
                value: serde_json::json!(5_i64),
            },
        ];
        let rows =
            DatabaseService::increment_field_where(&svc, "shares", "access_count", 1, &filters)
                .await
                .unwrap();
        assert_eq!(rows, 1, "exactly one row should match");

        let r = DatabaseService::get(&svc, "shares", "a").await.unwrap();
        assert_eq!(r.data["access_count"], serde_json::json!(1));
        let untouched = DatabaseService::get(&svc, "shares", "b").await.unwrap();
        assert_eq!(untouched.data["access_count"], serde_json::json!(0));
    }

    #[tokio::test]
    async fn increment_field_where_on_missing_table_returns_zero() {
        let svc = make_test_svc();
        let filters = vec![Filter {
            field: "id".into(),
            operator: FilterOp::Equal,
            value: serde_json::json!("nope"),
        }];
        let rows = DatabaseService::increment_field_where(
            &svc,
            "no_such_table",
            "access_count",
            1,
            &filters,
        )
        .await
        .unwrap();
        assert_eq!(rows, 0);
    }

    // -----------------------------------------------------------------------
    // upsert (INSERT … ON CONFLICT) — SetColumns + WindowedCounter
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn upsert_set_columns_inserts_then_updates_on_conflict() {
        use wafer_core::interfaces::database::service::{UpsertConflict, UpsertSpec};

        let svc = make_test_svc();
        let table = Table {
            name: "widgets".into(),
            columns: vec![pk("id"), Column::new("name", DataType::Text).null()],
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        };
        svc.ensure_schema_table(&table).await.unwrap();

        // No existing row on id=w1 → the ON CONFLICT insert lands as an insert.
        let n1 = DatabaseService::upsert(
            &svc,
            "widgets",
            UpsertSpec {
                data: vec![
                    ("id".into(), serde_json::json!("w1")),
                    ("name".into(), serde_json::json!("a")),
                ],
                conflict_columns: vec!["id".into()],
                on_conflict: UpsertConflict::SetColumns(vec!["name".into()]),
            },
        )
        .await
        .unwrap();
        assert_eq!(n1, 1, "insert affects one row");
        let r1 = DatabaseService::get(&svc, "widgets", "w1").await.unwrap();
        assert_eq!(r1.data["name"], serde_json::json!("a"));

        // Same id → conflict on the PK → DO UPDATE SET name = excluded.name.
        let n2 = DatabaseService::upsert(
            &svc,
            "widgets",
            UpsertSpec {
                data: vec![
                    ("id".into(), serde_json::json!("w1")),
                    ("name".into(), serde_json::json!("b")),
                ],
                conflict_columns: vec!["id".into()],
                on_conflict: UpsertConflict::SetColumns(vec!["name".into()]),
            },
        )
        .await
        .unwrap();
        assert_eq!(n2, 1, "conflict update affects one row");
        let r2 = DatabaseService::get(&svc, "widgets", "w1").await.unwrap();
        assert_eq!(
            r2.data["name"],
            serde_json::json!("b"),
            "on-conflict updated name a -> b"
        );

        let total = DatabaseService::count(&svc, "widgets", &[]).await.unwrap();
        assert_eq!(
            total, 1,
            "still exactly one row — the second call updated, not inserted"
        );
    }

    #[tokio::test]
    async fn upsert_windowed_counter_increments_in_window_and_keeps_created_at() {
        use wafer_core::interfaces::database::service::{UpsertConflict, UpsertSpec};

        let svc = make_test_svc();
        // Seed an existing counter row with SENTINEL timestamps so we can prove
        // created_at is immutable across conflict-updates (Task-5 fix): if the
        // builder wrongly re-stamped created_at in DO UPDATE SET, the sentinel
        // would be overwritten with CURRENT_TIMESTAMP. `key` is UNIQUE — the
        // conflict target.
        {
            let db = svc.db.lock().unwrap();
            db.execute_batch(
                "CREATE TABLE rl (
                     id TEXT PRIMARY KEY,
                     key TEXT UNIQUE,
                     count INTEGER,
                     window_start INTEGER,
                     created_at TEXT,
                     updated_at TEXT
                 );
                 INSERT INTO rl (id, key, count, window_start, created_at, updated_at)
                 VALUES ('seed', 'user:1:login', 1, 1700000000, 'SENTINEL-CREATED', 'SENTINEL-UPDATED');",
            )
            .unwrap();
        }

        let now = 1_700_000_000_i64;
        let cutoff = now - 60; // 60s window; stored window_start (=now) is NOT expired
        let make_spec = || UpsertSpec {
            data: vec![
                ("id".into(), serde_json::json!("fresh-id")),
                ("key".into(), serde_json::json!("user:1:login")),
            ],
            conflict_columns: vec!["key".into()],
            on_conflict: UpsertConflict::WindowedCounter {
                count_field: "count".into(),
                window_field: "window_start".into(),
                now,
                window_cutoff: cutoff,
                created_fields: vec!["created_at".into()],
                updated_fields: vec!["updated_at".into()],
            },
        };

        // First upsert conflicts on `key` → in-window increment (1 -> 2).
        let n1 = DatabaseService::upsert(&svc, "rl", make_spec())
            .await
            .unwrap();
        assert_eq!(n1, 1, "conflict update affects the one matching row");
        let r1 = DatabaseService::get(&svc, "rl", "seed").await.unwrap();
        assert_eq!(
            r1.data["count"],
            serde_json::json!(2),
            "count incremented 1 -> 2"
        );
        assert_eq!(
            r1.data["created_at"],
            serde_json::json!("SENTINEL-CREATED"),
            "created_at must be immutable on conflict (Task-5 fix)"
        );
        assert_ne!(
            r1.data["updated_at"],
            serde_json::json!("SENTINEL-UPDATED"),
            "updated_at must be re-stamped on conflict"
        );

        // Second in-window upsert → increments again (2 -> 3); created_at still untouched.
        let n2 = DatabaseService::upsert(&svc, "rl", make_spec())
            .await
            .unwrap();
        assert_eq!(n2, 1);
        let r2 = DatabaseService::get(&svc, "rl", "seed").await.unwrap();
        assert_eq!(
            r2.data["count"],
            serde_json::json!(3),
            "count incremented 2 -> 3 on the second in-window upsert"
        );
        assert_eq!(
            r2.data["created_at"],
            serde_json::json!("SENTINEL-CREATED"),
            "created_at still unchanged after the second in-window upsert"
        );

        // The conflicting upserts never inserted a duplicate row.
        let total = DatabaseService::count(&svc, "rl", &[]).await.unwrap();
        assert_eq!(total, 1, "no duplicate row was inserted");
    }

    // -----------------------------------------------------------------------
    // Lazy column-add on filtered writes (sqlite/postgres divergence resolved
    // deliberately: both backends now lazily add missing filter/data columns
    // on the *_where family, matching the documented lazy column-add design;
    // previously SQLite errored with "no such column").
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_where_lazily_adds_missing_filter_column() {
        let svc = make_test_svc();
        seed_rows(&svc, "items", vec![serde_json::json!({"name": "alpha"})]).await;

        // `archived` is not in the schema: the column is lazily added (NULL),
        // the filter matches nothing, and no error surfaces.
        let filters = vec![Filter {
            field: "archived".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("yes"),
        }];
        let count = DatabaseService::delete_where_count(&svc, "items", &filters)
            .await
            .expect("missing filter column must be lazily added, not error");
        assert_eq!(count, 0);

        // The row survives and the column now exists (NULL on the old row).
        let remaining = DatabaseService::list(&svc, "items", &ListOptions::default())
            .await
            .unwrap();
        assert_eq!(remaining.records.len(), 1);
        assert_eq!(
            remaining.records[0].data.get("archived"),
            Some(&serde_json::Value::Null)
        );
    }

    #[tokio::test]
    async fn update_where_lazily_adds_missing_data_and_filter_columns() {
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "items",
            vec![
                serde_json::json!({"name": "alpha"}),
                serde_json::json!({"name": "beta"}),
            ],
        )
        .await;

        // Neither `flag` (SET) nor `category` (WHERE) exists yet.
        let mut patch = std::collections::HashMap::new();
        patch.insert("flag".to_string(), serde_json::json!("on"));
        let filters = vec![Filter {
            field: "category".to_string(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        }];
        DatabaseService::update_where(&svc, "items", &filters, patch)
            .await
            .expect("missing SET/filter columns must be lazily added, not error");

        // Both rows match (category IS NULL after the lazy add) and got the flag.
        let rows = DatabaseService::list(&svc, "items", &ListOptions::default())
            .await
            .unwrap();
        assert_eq!(rows.records.len(), 2);
        for r in &rows.records {
            assert_eq!(r.data["flag"], serde_json::json!("on"));
        }
    }

    #[tokio::test]
    async fn take_where_lazily_adds_missing_filter_column() {
        let svc = make_test_svc();
        seed_rows(&svc, "codes", vec![serde_json::json!({"code": "abc"})]).await;
        let filters = vec![Filter {
            field: "claimed_by".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("nobody"),
        }];
        let taken = DatabaseService::take_where(&svc, "codes", &filters)
            .await
            .expect("missing filter column must be lazily added, not error");
        assert!(taken.is_empty());
        let remaining = DatabaseService::count(&svc, "codes", &[]).await.unwrap();
        assert_eq!(remaining, 1);
    }

    #[tokio::test]
    async fn create_stores_objects_as_json_and_roundtrips() {
        // Objects flow through the shared create default →
        // wafer_sql_utils::query::build_insert → Value::Json → TEXT bind on
        // SQLite; the read path parses JSON-looking text back into a value.
        let svc = make_test_svc();
        seed_rows(&svc, "items", vec![serde_json::json!({"name": "seed"})]).await;
        let mut data = std::collections::HashMap::new();
        data.insert("name".to_string(), serde_json::json!("with-meta"));
        data.insert("meta".to_string(), serde_json::json!({"a": 1, "b": [true]}));
        let created = DatabaseService::create(&svc, "items", data).await.unwrap();
        assert!(!created.id.is_empty());

        let reread = DatabaseService::get(&svc, "items", &created.id)
            .await
            .unwrap();
        assert_eq!(
            reread.data["meta"],
            serde_json::json!({"a": 1, "b": [true]})
        );
    }

    #[tokio::test]
    async fn create_on_integer_pk_table_returns_generated_rowid() {
        // INTEGER PRIMARY KEY tables generate their own id: create() must not
        // synthesize a UUID, and the rowid from the lock-spanning run_insert
        // is folded into the returned record.
        let svc = make_test_svc();
        {
            let db = svc.db.lock().unwrap();
            db.execute_batch(
                "CREATE TABLE counters (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
            )
            .unwrap();
        }
        let mut data = std::collections::HashMap::new();
        data.insert("name".to_string(), serde_json::json!("first"));
        let created = DatabaseService::create(&svc, "counters", data)
            .await
            .unwrap();
        assert_eq!(created.id, "1");
        assert_eq!(created.data["id"], serde_json::json!(1));

        let reread = DatabaseService::get(&svc, "counters", "1").await.unwrap();
        assert_eq!(reread.data["name"], serde_json::json!("first"));
    }

    #[tokio::test]
    async fn list_skip_count_returns_records_len_as_total_count() {
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "rows",
            vec![
                serde_json::json!({"name": "a"}),
                serde_json::json!({"name": "b"}),
                serde_json::json!({"name": "c"}),
                serde_json::json!({"name": "d"}),
                serde_json::json!({"name": "e"}),
            ],
        )
        .await;

        // With skip_count: true — total_count is records.len(), not full count.
        let opts_skip = ListOptions {
            limit: 2,
            skip_count: true,
            ..Default::default()
        };
        let result = DatabaseService::list(&svc, "rows", &opts_skip)
            .await
            .unwrap();
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.total_count, 2);

        // With skip_count: false — total_count is the full collection size.
        let opts_count = ListOptions {
            limit: 2,
            skip_count: false,
            ..Default::default()
        };
        let result = DatabaseService::list(&svc, "rows", &opts_count)
            .await
            .unwrap();
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.total_count, 5);
    }

    #[tokio::test]
    async fn list_with_column_projection_returns_only_selected_columns() {
        // `columns: Some([...])` renders `SELECT id, name` (not `SELECT *`),
        // so the unprojected `secret` column must be absent from the returned
        // record even though the row has a value for it.
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "rows",
            vec![serde_json::json!({"name": "a", "secret": "s1"})],
        )
        .await;
        let opts = ListOptions {
            columns: Some(vec!["id".into(), "name".into()]),
            ..Default::default()
        };
        let list = DatabaseService::list(&svc, "rows", &opts).await.unwrap();
        let row = &list.records[0].data;
        assert!(row.contains_key("name"), "projected column present");
        assert!(!row.contains_key("secret"), "unprojected column absent");
        // The projection is honored, but a `None` projection still returns
        // every column — sanity-check the fixture actually stored `secret`.
        let full = DatabaseService::list(&svc, "rows", &ListOptions::default())
            .await
            .unwrap();
        assert_eq!(full.records[0].data["secret"], serde_json::json!("s1"));
    }

    #[tokio::test]
    async fn list_with_any_group_filter_returns_only_or_matching_rows() {
        // A group filter (`Any` = OR) must actually execute against the DB via
        // `filter_tree` → `build_condition_tree` → `extra_condition`. Before
        // Task 4, LIST flattened the tree to empty and returned ALL rows (the
        // Task 3 fail-open). Here `status = 'active' OR status = 'pending'`
        // must return exactly the two matching rows and skip 'archived',
        // and `total_count` (computed with the same extra_condition) must
        // agree with the filtered set — not the full table.
        let svc = make_test_svc();
        seed_rows(
            &svc,
            "rows",
            vec![
                serde_json::json!({"name": "a", "status": "active"}),
                serde_json::json!({"name": "b", "status": "pending"}),
                serde_json::json!({"name": "c", "status": "archived"}),
                serde_json::json!({"name": "d", "status": "archived"}),
            ],
        )
        .await;

        let tree = vec![FilterTree::Any(vec![
            FilterTree::Leaf(Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!("active"),
            }),
            FilterTree::Leaf(Filter {
                field: "status".into(),
                operator: FilterOp::Equal,
                value: serde_json::json!("pending"),
            }),
        ])];
        let opts = ListOptions {
            filters: Vec::new(),
            filter_tree: Some(tree),
            sort: vec![SortField {
                field: "name".into(),
                desc: false,
            }],
            ..Default::default()
        };
        let list = DatabaseService::list(&svc, "rows", &opts).await.unwrap();

        assert_eq!(
            list.records.len(),
            2,
            "only the two OR-matching rows should return, not all four"
        );
        let statuses: Vec<&str> = list
            .records
            .iter()
            .map(|r| r.data["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, vec!["active", "pending"]);
        assert!(
            !statuses.contains(&"archived"),
            "archived rows must be excluded by the group filter"
        );
        assert_eq!(
            list.total_count, 2,
            "total_count must reflect the filtered set (extra_condition applied to COUNT), not the full table"
        );
    }

    #[tokio::test]
    async fn list_with_group_filter_on_column_absent_from_schema_lazily_adds_it() {
        // A field that appears ONLY inside a group (`filter_tree`), never in
        // the flat `filters` list, must still get its lazy TEXT column added
        // by `ensure_query_columns` (which now walks the tree's leaves). If it
        // didn't, the SELECT would fail with "no such column".
        let svc = make_test_svc();
        seed_rows(&svc, "rows", vec![serde_json::json!({"name": "a"})]).await;

        let tree = vec![FilterTree::Any(vec![FilterTree::Leaf(Filter {
            field: "tier".into(), // absent from the seeded schema
            operator: FilterOp::Equal,
            value: serde_json::json!("gold"),
        })])];
        let opts = ListOptions {
            filter_tree: Some(tree),
            ..Default::default()
        };
        let list = DatabaseService::list(&svc, "rows", &opts)
            .await
            .expect("group-only filter column must be lazily added, not error");
        // No row has tier='gold' (the column was just added as NULL), so the
        // filter matches nothing — but the query must succeed.
        assert!(list.records.is_empty());
        assert_eq!(list.total_count, 0);
    }

    #[tokio::test]
    async fn get_missing_row_returns_not_found() {
        let svc = make_test_svc();
        seed_rows(&svc, "widgets", vec![serde_json::json!({"name": "a"})]).await;
        let err = DatabaseService::get(&svc, "widgets", "no-such-id")
            .await
            .unwrap_err();
        assert!(matches!(err, DatabaseError::NotFound));
    }

    #[tokio::test]
    async fn count_on_missing_table_returns_zero() {
        let svc = make_test_svc();
        let n = DatabaseService::count(&svc, "no_such_table", &[])
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn update_lazily_adds_a_column_absent_from_the_schema() {
        // Regression for the M18 fix: SQLite `update` ensures columns from
        // the update payload (matching Postgres) — now via the shared
        // `DbExec::ensure_data_columns` default. Previously updating a key
        // absent from the table failed with a confusing "no such column";
        // now the column is added.
        let svc = make_test_svc();
        seed_rows(&svc, "widgets", vec![serde_json::json!({"name": "a"})]).await;
        let created = DatabaseService::list(&svc, "widgets", &ListOptions::default())
            .await
            .unwrap();
        let id = created.records[0].id.clone();

        let mut patch = std::collections::HashMap::new();
        // `nickname` is not in the seeded schema.
        patch.insert("nickname".to_string(), serde_json::json!("ace"));
        let updated = DatabaseService::update(&svc, "widgets", &id, patch)
            .await
            .expect("update should add the missing column and succeed");
        assert_eq!(updated.data["nickname"], serde_json::json!("ace"));

        // The column now exists and round-trips on a fresh read.
        let reread = DatabaseService::get(&svc, "widgets", &id).await.unwrap();
        assert_eq!(reread.data["nickname"], serde_json::json!("ace"));
    }

    #[tokio::test]
    async fn ensure_query_columns_propagates_error_on_missing_table() {
        // Regression for the M17 fix: `table_columns` now surfaces a real DB
        // error instead of `Err(())` collapsed to "no columns". A
        // `PRAGMA table_info` on a non-existent table returns no rows (not an
        // error), so this still succeeds — but a malformed statement would now
        // propagate. Here we assert the success-with-empty-rows contract is
        // preserved so callers don't regress.
        let svc = make_test_svc();
        let filters = vec![Filter {
            field: "whatever".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("x"),
        }];
        // No such table: PRAGMA table_info yields zero rows, so the missing
        // filter column would be "added" against a table that doesn't exist,
        // which surfaces as a real DDL error rather than being swallowed.
        let res = svc
            .ensure_query_columns("no_such_table", &filters, &[], None)
            .await;
        assert!(
            res.is_err(),
            "adding a column to a non-existent table must surface an error, not be swallowed"
        );
    }

    #[tokio::test]
    async fn schema_table_exists_reflects_creation() {
        let svc = make_test_svc();
        assert!(!DatabaseService::schema_table_exists(&svc, "widgets")
            .await
            .unwrap());
        seed_rows(&svc, "widgets", vec![serde_json::json!({"name": "a"})]).await;
        assert!(DatabaseService::schema_table_exists(&svc, "widgets")
            .await
            .unwrap());
    }
}
