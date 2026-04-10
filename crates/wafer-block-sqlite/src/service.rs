use rusqlite::{types::Value as SqlValue, Connection, Row};
use std::collections::HashMap;
use std::sync::Mutex;

use wafer_core::interfaces::database::service::*;
use wafer_sql_utils::base64::base64_encode;
use wafer_sql_utils::ddl;
use wafer_sql_utils::ident::sanitize_ident;
use wafer_sql_utils::value::sea_values_to_json;
use wafer_sql_utils::Backend;

/// SQLite implementation of the DatabaseService.
pub struct SQLiteDatabaseService {
    db: Mutex<Connection>,
}

impl SQLiteDatabaseService {
    pub fn new(db: Connection) -> Self {
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

    pub fn open(path: &str) -> Result<Self, DatabaseError> {
        let conn = Connection::open(path)
            .map_err(|e| DatabaseError::Internal(format!("open database: {}", e)))?;
        Ok(Self::new(conn))
    }

    pub fn open_in_memory() -> Result<Self, DatabaseError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| DatabaseError::Internal(format!("open in-memory database: {}", e)))?;
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

/// Auto-create a table with columns matching the provided data keys.
/// Uses TEXT type for all columns (SQLite is dynamically typed anyway).
/// The `id` column is used as the primary key.
fn ensure_table(db: &Connection, table: &str, data: &HashMap<String, serde_json::Value>) {
    let safe_table = sanitize_ident(table);
    let mut col_defs = Vec::new();
    for key in data.keys() {
        let safe_key = sanitize_ident(key);
        if key == "id" {
            col_defs.insert(0, "id TEXT PRIMARY KEY".to_string());
        } else {
            col_defs.push(format!("{} TEXT", safe_key));
        }
    }
    if !data.contains_key("id") {
        col_defs.insert(0, "id TEXT PRIMARY KEY".to_string());
    }
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {} ({})",
        safe_table,
        col_defs.join(", ")
    );
    db.execute_batch(&sql).ok();

    // Also ensure any missing columns are added (for when a table exists but new fields are inserted)
    if let Ok(existing) = table_columns(db, &safe_table) {
        for key in data.keys() {
            let safe_key = sanitize_ident(key);
            if !existing.contains(&safe_key.to_lowercase()) {
                let alter = format!("ALTER TABLE {} ADD COLUMN {} TEXT", safe_table, safe_key);
                db.execute_batch(&alter).ok();
            }
        }
    }
}

/// Get list of column names for an existing table.
fn table_columns(db: &Connection, table: &str) -> Result<Vec<String>, ()> {
    let safe_table = sanitize_ident(table);
    let mut stmt = db
        .prepare(&format!("PRAGMA table_info({})", safe_table))
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
    let mut stmt = match db.prepare(&format!("PRAGMA table_info(\"{}\")", safe_table)) {
        Ok(s) => s,
        Err(_) => return false,
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
                let alter = format!("ALTER TABLE {} ADD COLUMN {} TEXT", safe_table, safe_field);
                db.execute_batch(&alter).ok();
            }
        }
        for s in sort {
            let safe_field = sanitize_ident(&s.field);
            if !existing.contains(&safe_field.to_lowercase()) {
                let alter = format!("ALTER TABLE {} ADD COLUMN {} TEXT", safe_table, safe_field);
                db.execute_batch(&alter).ok();
            }
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl DatabaseService for SQLiteDatabaseService {
    async fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let table = sanitize_ident(collection);
        let sql = format!("SELECT * FROM {} WHERE id = ?1", table);
        db.query_row(&sql, [id], Self::row_to_record)
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

        // Count total
        let (count_sql, count_sea_vals) =
            wafer_sql_utils::aggregate::build_count(collection, &opts.filters, Backend::Sqlite);
        let count_params = sea_to_sql_params(count_sea_vals);
        let count_refs: Vec<&dyn rusqlite::types::ToSql> = count_params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        let total_count: i64 = db
            .query_row(&count_sql, count_refs.as_slice(), |row| row.get(0))
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        // Query records
        let (sql, sea_vals) =
            wafer_sql_utils::query::build_select(collection, opts, Backend::Sqlite);
        let params = sea_to_sql_params(sea_vals);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();

        let mut stmt = db
            .prepare(&sql)
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

        let page = if opts.limit > 0 {
            (opts.offset / opts.limit) + 1
        } else {
            1
        };

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

        // Auto-create table if it doesn't exist
        ensure_table(&db, &table, &data);

        let columns: Vec<&String> = data.keys().collect();
        let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("?{}", i)).collect();
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

            let set_clauses: Vec<String> = data
                .keys()
                .enumerate()
                .map(|(i, k)| format!("{} = ?{}", sanitize_ident(k), i + 1))
                .collect();

            let mut values: Vec<SqlValue> = data.values().map(json_to_sql_value).collect();
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
        let table = sanitize_ident(collection);
        let sql = format!("DELETE FROM {} WHERE id = ?1", table);
        let rows = db
            .execute(&sql, [id])
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
        let (sql, sea_vals) =
            wafer_sql_utils::aggregate::build_count(&table, filters, Backend::Sqlite);
        let params = sea_to_sql_params(sea_vals);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        db.query_row(&sql, query_params.as_slice(), |row| row.get(0))
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
        let (sql, sea_vals) =
            wafer_sql_utils::aggregate::build_sum(&table, field, filters, Backend::Sqlite);
        let params = sea_to_sql_params(sea_vals);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        db.query_row(&sql, query_params.as_slice(), |row| row.get(0))
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
        let (sql, sea_vals) =
            wafer_sql_utils::query::build_delete_where(&table, filters, Backend::Sqlite);
        let params = sea_to_sql_params(sea_vals);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        db.execute(&sql, query_params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        Ok(())
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

        let data_pairs: Vec<(String, serde_json::Value)> = data.into_iter().collect();
        let (sql, sea_vals) = wafer_sql_utils::query::build_update_where(
            &table,
            &data_pairs,
            filters,
            Backend::Sqlite,
        );
        let params = sea_to_sql_params(sea_vals);
        let query_params: Vec<&dyn rusqlite::types::ToSql> = params
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        db.execute(&sql, query_params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        Ok(())
    }

    // --- Schema management ---

    async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let sql = ddl::build_create_table(table, Backend::Sqlite);
        db.execute_batch(&sql)
            .map_err(|e| DatabaseError::Internal(format!("create table {}: {}", table.name, e)))?;

        // Add any missing columns
        if let Ok(existing) = table_columns(&db, &table.name) {
            for col in &table.columns {
                if !existing.contains(&col.name.to_lowercase()) {
                    let alter = ddl::build_add_column(&table.name, col, Backend::Sqlite);
                    if let Err(e) = db.execute_batch(&alter) {
                        tracing::warn!(table = %table.name, column = %col.name, error = %e, "failed to add column");
                    }
                }
            }
        }

        // Ensure indexes
        for idx in &table.indexes {
            let sql = ddl::build_create_index(&table.name, idx, Backend::Sqlite);
            db.execute_batch(&sql)
                .map_err(|e| DatabaseError::Internal(format!("create index: {}", e)))?;
        }

        // Create indexes for columns with foreign keys
        for col in &table.columns {
            if col.references.is_some() {
                let tbl = sanitize_ident(&table.name);
                let c = sanitize_ident(&col.name);
                let idx_name = format!("idx_{}_{}", tbl, c);
                let sql = format!("CREATE INDEX IF NOT EXISTS {} ON {}({})", idx_name, tbl, c);
                db.execute_batch(&sql)
                    .map_err(|e| DatabaseError::Internal(format!("create FK index: {}", e)))?;
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
        db.execute_batch(&ddl::build_drop_table(name, Backend::Sqlite))
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError> {
        let db = self
            .db
            .lock()
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let sql = ddl::build_add_column(table, column, Backend::Sqlite);
        db.execute_batch(&sql)
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wafer_core::interfaces::database::service::{Filter, FilterOp, ListOptions, SortField};

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
            json_to_sql_value(&serde_json::json!(3.14)),
            SqlValue::Real(3.14)
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
            other => panic!("expected Text, got {:?}", other),
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
            other => panic!("expected Text, got {:?}", other),
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
            SeaValue::Double(Some(3.14)),
            SeaValue::Bool(Some(true)),
            SeaValue::String(None), // NULL
        ];
        let params = sea_to_sql_params(sea_vals);
        assert_eq!(params.len(), 5);
        assert_eq!(params[0], SqlValue::Text("hello".to_string()));
        assert_eq!(params[1], SqlValue::Integer(42));
        assert_eq!(params[2], SqlValue::Real(3.14));
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
        };
        let (sql, sea_vals) = wafer_sql_utils::query::build_select("users", &opts, Backend::Sqlite);
        assert!(sql.contains("WHERE"));
        // SQLite uses ? placeholders, not $N
        assert!(sql.contains("?"), "SQLite should use ? placeholders");
        assert!(!sql.contains("$1"), "SQLite should not use $N placeholders");
        let params = sea_to_sql_params(sea_vals);
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
        };
        let (sql, _) = wafer_sql_utils::query::build_select("items", &opts, Backend::Sqlite);
        assert!(sql.contains("ORDER BY"));
        assert!(sql.contains("LIMIT"));
        assert!(sql.contains("OFFSET"));
    }

    #[test]
    fn test_sea_query_count_with_filters() {
        let filters = vec![Filter {
            field: "active".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!(true),
        }];
        let (sql, sea_vals) =
            wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Sqlite);
        assert!(sql.contains("COUNT(*)"));
        assert!(sql.contains("WHERE"));
        let params = sea_to_sql_params(sea_vals);
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_sea_query_sum() {
        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::json!("active"),
        }];
        let (sql, sea_vals) =
            wafer_sql_utils::aggregate::build_sum("orders", "amount", &filters, Backend::Sqlite);
        assert!(sql.contains("SUM"));
        assert!(sql.contains("COALESCE"));
        assert!(sql.contains("WHERE"));
        let params = sea_to_sql_params(sea_vals);
        assert!(!params.is_empty());
    }

    #[test]
    fn test_sea_query_delete_where() {
        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::In,
            value: serde_json::json!(["active", "pending"]),
        }];
        let (sql, sea_vals) =
            wafer_sql_utils::query::build_delete_where("users", &filters, Backend::Sqlite);
        assert!(sql.contains("DELETE FROM"));
        assert!(sql.contains("IN"));
        let params = sea_to_sql_params(sea_vals);
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
        let (sql, sea_vals) =
            wafer_sql_utils::query::build_update_where("users", &data, &filters, Backend::Sqlite);
        assert!(sql.contains("UPDATE"));
        assert!(sql.contains("SET"));
        assert!(sql.contains("WHERE"));
        let params = sea_to_sql_params(sea_vals);
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_sea_query_is_null_filter() {
        let filters = vec![Filter {
            field: "deleted_at".to_string(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        }];
        let (sql, sea_vals) =
            wafer_sql_utils::query::build_delete_where("users", &filters, Backend::Sqlite);
        assert!(sql.contains("IS NULL"));
        let params = sea_to_sql_params(sea_vals);
        assert!(params.is_empty());
    }

    #[test]
    fn test_sea_query_is_not_null_filter() {
        let filters = vec![Filter {
            field: "email".to_string(),
            operator: FilterOp::IsNotNull,
            value: serde_json::Value::Null,
        }];
        let (sql, sea_vals) =
            wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Sqlite);
        assert!(sql.contains("IS NOT NULL"));
        let params = sea_to_sql_params(sea_vals);
        assert!(params.is_empty());
    }

    #[test]
    fn test_sea_query_like_filter() {
        let filters = vec![Filter {
            field: "name".to_string(),
            operator: FilterOp::Like,
            value: serde_json::json!("%alice%"),
        }];
        let (sql, _sea_vals) =
            wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Sqlite);
        assert!(sql.contains("LIKE"));
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
        let (sql, sea_vals) =
            wafer_sql_utils::aggregate::build_count("users", &filters, Backend::Sqlite);
        assert!(sql.contains(">="));
        assert!(sql.contains("<"));
        let params = sea_to_sql_params(sea_vals);
        assert_eq!(params.len(), 2);
    }
}
