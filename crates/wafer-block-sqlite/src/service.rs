use std::{collections::HashMap, sync::Mutex};

use rusqlite::{types::Value as SqlValue, Connection, Row};
use wafer_block::db::{Filter, ListOptions, SortField};
use wafer_block_macro::wafer_async_trait;
#[cfg(test)]
use wafer_core::interfaces::database::service::{pk, DataType};
use wafer_core::interfaces::database::service::{
    Column, DatabaseError, DatabaseService, Record, RecordList, Table,
};
use wafer_sql_utils::{
    base64::base64_encode, ddl, ident::sanitize_ident, value::sea_values_to_json, Backend,
};

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
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null),
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
                    serde_json::Value::String(base64_encode(b))
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

/// Convert sea_query::Value params (from wafer-sql-utils builders) to rusqlite params
/// via JSON round-trip: sea_query::Value -> serde_json::Value -> rusqlite::types::Value.
fn sea_to_sql_params(sea_vals: Vec<wafer_sql_utils::SeaValue>) -> Vec<SqlValue> {
    sea_values_to_json(sea_vals)
        .iter()
        .map(json_to_sql_value)
        .collect()
}

fn json_to_sql_value(v: &serde_json::Value) -> SqlValue {
    match v {
        serde_json::Value::Null => SqlValue::Null,
        serde_json::Value::Bool(b) => SqlValue::Integer(if *b { 1 } else { 0 }),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SqlValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                SqlValue::Real(f)
            } else {
                SqlValue::Text(n.to_string())
            }
        }
        serde_json::Value::String(s) => SqlValue::Text(s.clone()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => SqlValue::Text(v.to_string()),
    }
}

/// For each key in `data` that isn't already a column on `table`, run
/// `ALTER TABLE ... ADD COLUMN ... TEXT`. Mirrors the lazy column-add
/// behaviour postgres + D1 keep on the `create` path — the table itself
/// must already exist via the block's migration files, but new columns
/// supplied through `data` are added on demand.
fn ensure_columns_from_data(
    db: &Connection,
    table: &str,
    data: &HashMap<String, serde_json::Value>,
) {
    let safe_table = sanitize_ident(table);
    let Ok(existing) = table_columns(db, &safe_table) else {
        return;
    };
    for key in data.keys() {
        let safe_key = sanitize_ident(key);
        if !existing.contains(&safe_key.to_lowercase()) {
            let alter = format!("ALTER TABLE {safe_table} ADD COLUMN {safe_key} TEXT");
            db.execute_batch(&alter).ok();
        }
    }
}

/// Get list of column names for an existing table.
fn table_columns(db: &Connection, table: &str) -> Result<Vec<String>, ()> {
    let safe_table = sanitize_ident(table);
    let mut stmt = db
        .prepare(&format!("PRAGMA table_info({safe_table})"))
        .map_err(|_| ())?;
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|_| ())?
        .filter_map(|r| r.ok())
        .map(|c| c.to_lowercase())
        .collect();
    Ok(cols)
}

/// Check if the table's `id` column is INTEGER PRIMARY KEY (autoincrement).
fn has_integer_pk(db: &Connection, table: &str) -> bool {
    let safe_table = sanitize_ident(table);
    let Ok(mut stmt) = db.prepare(&format!("PRAGMA table_info(\"{safe_table}\")")) else {
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

/// Check if a table exists in the database.
fn table_exists(db: &Connection, table: &str) -> bool {
    db.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
}

/// Ensure that columns referenced in filters and sorts exist on the table.
/// Adds missing columns as TEXT (they'll default to NULL).
fn ensure_columns_for_query(db: &Connection, table: &str, filters: &[Filter], sort: &[SortField]) {
    let safe_table = sanitize_ident(table);
    if let Ok(existing) = table_columns(db, &safe_table) {
        for f in filters {
            let safe_field = sanitize_ident(&f.field);
            if !existing.contains(&safe_field.to_lowercase()) {
                let alter = format!("ALTER TABLE {safe_table} ADD COLUMN {safe_field} TEXT");
                db.execute_batch(&alter).ok();
            }
        }
        for s in sort {
            let safe_field = sanitize_ident(&s.field);
            if !existing.contains(&safe_field.to_lowercase()) {
                let alter = format!("ALTER TABLE {safe_table} ADD COLUMN {safe_field} TEXT");
                db.execute_batch(&alter).ok();
            }
        }
    }
}

#[wafer_async_trait]
impl DatabaseService for SQLiteDatabaseService {
    async fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let stmt = wafer_sql_utils::query::build_select_by_id(collection, id, Backend::Sqlite);
        let params = sea_to_sql_params(stmt.values);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        db.query_row(&stmt.sql, query_params.as_slice(), Self::row_to_record)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => DatabaseError::NotFound,
                _ => DatabaseError::Internal(e.to_string()),
            })
    }

    async fn list(
        &self,
        collection: &str,
        opts: &ListOptions,
    ) -> Result<RecordList, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let collection = &sanitize_ident(collection);
        if !table_exists(&db, collection) {
            return Ok(RecordList {
                records: Vec::new(),
                total_count: 0,
                page: 1,
                page_size: if opts.limit > 0 { opts.limit } else { 0 },
            });
        }

        // Ensure filter/sort columns exist (add them if missing)
        ensure_columns_for_query(&db, collection, &opts.filters, &opts.sort);

        // Count total — skipped when opts.skip_count is set (the caller has
        // signalled they only care about the records and will not read
        // total_count as the full collection size). We then synthesize
        // total_count from records.len() below.
        let total_count: Option<i64> = if opts.skip_count {
            None
        } else {
            let count_stmt =
                wafer_sql_utils::aggregate::build_count(collection, &opts.filters, Backend::Sqlite);
            let count_params = sea_to_sql_params(count_stmt.values);
            let count_refs: Vec<&dyn rusqlite::types::ToSql> = count_params
                .iter()
                .map(|v| v as &dyn rusqlite::types::ToSql)
                .collect();
            Some(
                db.query_row(&count_stmt.sql, count_refs.as_slice(), |row| row.get(0))
                    .map_err(|e| DatabaseError::Internal(e.to_string()))?,
            )
        };

        // Query records
        let stmt = wafer_sql_utils::query::build_select(collection, opts, Backend::Sqlite);
        let params = sea_to_sql_params(stmt.values);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let mut prepared = db
            .prepare(&stmt.sql)
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        let records: Vec<Record> = prepared
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

        let page = if opts.limit > 0 {
            (opts.offset / opts.limit) + 1
        } else {
            1
        };

        let total_count = total_count.unwrap_or(records.len() as i64);
        Ok(RecordList {
            records,
            total_count,
            page,
            page_size: if opts.limit > 0 {
                opts.limit
            } else {
                total_count
            },
        })
    }

    async fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let table = sanitize_ident(collection);

        let mut data = data;

        // Auto-generate ID if not provided, but only for string/UUID PKs.
        // Tables with INTEGER PRIMARY KEY AUTOINCREMENT should not get a
        // UUID — let SQLite handle the autoincrement.
        if !data.contains_key("id") && !has_integer_pk(&db, &table) {
            data.insert(
                "id".to_string(),
                serde_json::Value::String(uuid::Uuid::new_v4().to_string()),
            );
        }

        // Auto-set timestamps
        let now = chrono::Utc::now().to_rfc3339();
        if !data.contains_key("created_at") {
            data.insert(
                "created_at".to_string(),
                serde_json::Value::String(now.clone()),
            );
        }
        if !data.contains_key("updated_at") {
            data.insert("updated_at".to_string(), serde_json::Value::String(now));
        }

        // Ensure any new columns exist (matches the postgres + D1 `create`
        // paths). Table creation itself is the block migration's job.
        ensure_columns_from_data(&db, &table, &data);

        // Sorted-key iteration so the generated INSERT is stable across
        // process starts. HashMap order is randomized by RandomState, which
        // would otherwise produce N permutations of the same INSERT — each
        // a distinct cached prepared statement.
        let mut columns: Vec<&String> = data.keys().collect();
        columns.sort();
        let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("?{i}")).collect();
        let values: Vec<SqlValue> = columns
            .iter()
            .map(|k| json_to_sql_value(&data[*k]))
            .collect();

        let safe_col_names: Vec<String> = columns.iter().map(|c| sanitize_ident(c)).collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            table,
            safe_col_names.join(", "),
            placeholders.join(", ")
        );

        let params: Vec<&dyn rusqlite::types::ToSql> = values
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        db.execute(&sql, params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        let id = match data.get("id") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => {
                // For autoincrement tables, retrieve the generated id
                let rowid = db.last_insert_rowid();
                let id_str = rowid.to_string();
                data.insert("id".to_string(), serde_json::json!(rowid));
                id_str
            }
        };

        Ok(Record { id, data })
    }

    async fn update(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        {
            let db = self
                .db
                .lock()
                .map_err(|e| DatabaseError::Internal(e.to_string()))?;
            let table = sanitize_ident(collection);

            let mut data = data;

            // Auto-update timestamp
            if !data.contains_key("updated_at") {
                data.insert(
                    "updated_at".to_string(),
                    serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                );
            }

            // Sorted-key iteration so the generated SET clause is stable —
            // see `create()` above for the rationale.
            let mut keys: Vec<&String> = data.keys().collect();
            keys.sort();
            let set_clauses: Vec<String> = keys
                .iter()
                .enumerate()
                .map(|(i, k)| format!("{} = ?{}", sanitize_ident(k), i + 1))
                .collect();

            let mut values: Vec<SqlValue> =
                keys.iter().map(|k| json_to_sql_value(&data[*k])).collect();
            values.push(SqlValue::Text(id.to_string()));

            let sql = format!(
                "UPDATE {} SET {} WHERE id = ?{}",
                table,
                set_clauses.join(", "),
                values.len()
            );

            let params: Vec<&dyn rusqlite::types::ToSql> = values
                .iter()
                .map(|v| v as &dyn rusqlite::types::ToSql)
                .collect();

            let rows = db
                .execute(&sql, params.as_slice())
                .map_err(|e| DatabaseError::Internal(e.to_string()))?;

            if rows == 0 {
                return Err(DatabaseError::NotFound);
            }
        }

        // Fetch the updated record
        self.get(collection, id).await
    }

    async fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let stmt = wafer_sql_utils::query::build_delete_by_id(collection, id, Backend::Sqlite);
        let params = sea_to_sql_params(stmt.values);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = db
            .execute(&stmt.sql, query_params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        if rows == 0 {
            return Err(DatabaseError::NotFound);
        }
        Ok(())
    }

    async fn count(&self, collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let table = sanitize_ident(collection);
        if !table_exists(&db, &table) {
            return Ok(0);
        }
        ensure_columns_for_query(&db, &table, filters, &[]);
        let stmt = wafer_sql_utils::aggregate::build_count(&table, filters, Backend::Sqlite);
        let params = sea_to_sql_params(stmt.values);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        db.query_row(&stmt.sql, query_params.as_slice(), |row| row.get(0))
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    async fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let table = sanitize_ident(collection);
        let stmt = wafer_sql_utils::aggregate::build_sum(&table, field, filters, Backend::Sqlite);
        let params = sea_to_sql_params(stmt.values);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        db.query_row(&stmt.sql, query_params.as_slice(), |row| row.get(0))
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    async fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let params: Vec<SqlValue> = args.iter().map(json_to_sql_value).collect();
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let mut stmt = db
            .prepare(query)
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        let records: Vec<Record> = stmt
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

        Ok(records)
    }

    async fn exec_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let params: Vec<SqlValue> = args.iter().map(json_to_sql_value).collect();
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let rows = db
            .execute(query, query_params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        Ok(rows as i64)
    }

    async fn delete_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<(), DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let table = sanitize_ident(collection);
        if !table_exists(&db, &table) {
            return Ok(());
        }
        let stmt = wafer_sql_utils::query::build_delete_where(&table, filters, Backend::Sqlite);
        let params = sea_to_sql_params(stmt.values);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        db.execute(&stmt.sql, query_params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn delete_where_count(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let table = sanitize_ident(collection);
        if !table_exists(&db, &table) {
            return Ok(0);
        }
        let stmt = wafer_sql_utils::query::build_delete_where(&table, filters, Backend::Sqlite);
        let params = sea_to_sql_params(stmt.values);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        let affected = db
            .execute(&stmt.sql, query_params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        Ok(affected as i64)
    }

    async fn take_where(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<Vec<Record>, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let table = sanitize_ident(collection);
        if !table_exists(&db, &table) {
            return Ok(vec![]);
        }
        let stmt =
            wafer_sql_utils::query::build_delete_where_returning(&table, filters, Backend::Sqlite);
        let params = sea_to_sql_params(stmt.values);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        let mut prepared = db
            .prepare(&stmt.sql)
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let records: Vec<Record> = prepared
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
        Ok(records)
    }

    async fn update_where(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let table = sanitize_ident(collection);
        if !table_exists(&db, &table) {
            return Err(DatabaseError::NotFound);
        }

        let mut data = data;
        if !data.contains_key("updated_at") {
            data.insert(
                "updated_at".to_string(),
                serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
            );
        }

        // Sort by key so the generated SET clause is stable across runs —
        // see `create()` above for the rationale (HashMap iteration order
        // would otherwise produce a fresh prepared statement per run).
        let mut data_pairs: Vec<(String, serde_json::Value)> = data.into_iter().collect();
        data_pairs.sort_by(|a, b| a.0.cmp(&b.0));
        let stmt = wafer_sql_utils::query::build_update_where(
            &table,
            &data_pairs,
            filters,
            Backend::Sqlite,
        );
        let params = sea_to_sql_params(stmt.values);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        db.execute(&stmt.sql, query_params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        Ok(())
    }

    async fn increment_field_where(
        &self,
        collection: &str,
        col: &str,
        delta: i64,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let table = sanitize_ident(collection);
        if !table_exists(&db, &table) {
            return Ok(0);
        }
        let stmt = wafer_sql_utils::query::build_increment_field_where(
            &table,
            col,
            delta,
            filters,
            Backend::Sqlite,
        );
        let params = sea_to_sql_params(stmt.values);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = db
            .execute(&stmt.sql, query_params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        Ok(rows as i64)
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

        // Add any missing columns
        if let Ok(existing) = table_columns(&db, &table.name) {
            for col in &table.columns {
                if !existing.contains(&col.name.to_lowercase()) {
                    let alter = ddl::build_add_column(&table.name, col, Backend::Sqlite);
                    if let Err(e) = db.execute_batch(&alter.sql) {
                        tracing::warn!(table = %table.name, column = %col.name, error = %e, "failed to add column");
                    }
                }
            }
        }

        // Ensure indexes
        for idx in &table.indexes {
            let idx_stmt = ddl::build_create_index(&table.name, idx, Backend::Sqlite);
            db.execute_batch(&idx_stmt.sql)
                .map_err(|e| DatabaseError::Internal(format!("create index: {e}")))?;
        }

        // Create indexes for columns with foreign keys
        for col in &table.columns {
            if col.references.is_some() {
                let tbl = sanitize_ident(&table.name);
                let c = sanitize_ident(&col.name);
                let idx_name = format!("idx_{tbl}_{c}");
                let sql = format!("CREATE INDEX IF NOT EXISTS {idx_name} ON {tbl}({c})");
                db.execute_batch(&sql)
                    .map_err(|e| DatabaseError::Internal(format!("create FK index: {e}")))?;
            }
        }

        Ok(())
    }

    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        Ok(table_exists(&db, name))
    }

    async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        db.execute_batch(&ddl::build_drop_table(name, Backend::Sqlite).sql)
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let stmt = ddl::build_add_column(table, column, Backend::Sqlite);
        db.execute_batch(&stmt.sql)
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};

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
    // sea_to_sql_params bridge tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_sea_to_sql_params_mixed_types() {
        use wafer_sql_utils::SeaValue;
        let sea_vals = vec![
            SeaValue::String(Some(Box::new("hello".to_string()))),
            SeaValue::BigInt(Some(42)),
            SeaValue::Double(Some(2.5)),
            SeaValue::Bool(Some(true)),
            SeaValue::String(None), // NULL
        ];
        let params = sea_to_sql_params(sea_vals);
        assert_eq!(params.len(), 5);
        assert_eq!(params[0], SqlValue::Text("hello".to_string()));
        assert_eq!(params[1], SqlValue::Integer(42));
        assert_eq!(params[2], SqlValue::Real(2.5));
        assert_eq!(params[3], SqlValue::Integer(1)); // bool true → 1
        assert_eq!(params[4], SqlValue::Null);
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
        };
        let stmt = wafer_sql_utils::query::build_select("users", &opts, Backend::Sqlite);
        let sql = stmt.sql;
        assert!(sql.contains("WHERE"));
        // SQLite uses ? placeholders, not $N
        assert!(sql.contains("?"), "SQLite should use ? placeholders");
        assert!(!sql.contains("$1"), "SQLite should not use $N placeholders");
        let params = sea_to_sql_params(stmt.values);
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
        let params = sea_to_sql_params(stmt.values);
        assert_eq!(params.len(), 1);
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
        let params = sea_to_sql_params(stmt.values);
        assert!(!params.is_empty());
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
        let params = sea_to_sql_params(stmt.values);
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
        let params = sea_to_sql_params(stmt.values);
        assert_eq!(params.len(), 2);
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
        let params = sea_to_sql_params(stmt.values);
        assert!(params.is_empty());
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
        let params = sea_to_sql_params(stmt.values);
        assert!(params.is_empty());
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
        let params = sea_to_sql_params(stmt.values);
        assert_eq!(params.len(), 2);
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
            svc.create(collection, data).await.unwrap();
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

        let count = svc.delete_where_count("items", &filters).await.unwrap();
        assert_eq!(count, 2, "should have deleted exactly 2 active rows");

        // Remaining row is the inactive one
        let remaining = svc.count("items", &[]).await.unwrap();
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

        let count = svc.delete_where_count("items", &filters).await.unwrap();
        assert_eq!(count, 0);

        // Row still exists
        let remaining = svc.count("items", &[]).await.unwrap();
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
        let count = svc
            .delete_where_count("no_such_table", &filters)
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

        let taken = svc.take_where("codes", &filters).await.unwrap();
        assert_eq!(taken.len(), 2, "should have taken 2 unused codes");

        // Verify the rows are actually deleted
        let remaining = svc.count("codes", &[]).await.unwrap();
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

        let taken = svc.take_where("codes", &filters).await.unwrap();
        assert!(taken.is_empty());

        // Original row still present
        let remaining = svc.count("codes", &[]).await.unwrap();
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
        let taken = svc.take_where("no_such_table", &filters).await.unwrap();
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
            svc.create("shares", row).await.unwrap();
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
        let rows = svc
            .increment_field_where("shares", "access_count", 1, &filters)
            .await
            .unwrap();
        assert_eq!(rows, 1, "exactly one row should match");

        let r = svc.get("shares", "a").await.unwrap();
        assert_eq!(r.data["access_count"], serde_json::json!(1));
        let untouched = svc.get("shares", "b").await.unwrap();
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
        let rows = svc
            .increment_field_where("no_such_table", "access_count", 1, &filters)
            .await
            .unwrap();
        assert_eq!(rows, 0);
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
        let result = svc.list("rows", &opts_skip).await.unwrap();
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.total_count, 2);

        // With skip_count: false — total_count is the full collection size.
        let opts_count = ListOptions {
            limit: 2,
            skip_count: false,
            ..Default::default()
        };
        let result = svc.list("rows", &opts_count).await.unwrap();
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.total_count, 5);
    }
}
