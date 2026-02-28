use rusqlite::{types::Value as SqlValue, Connection, Row};
use std::collections::HashMap;
use std::sync::Mutex;

use super::database::*;

/// SQLite implementation of the DatabaseService.
pub struct SQLiteDatabaseService {
    db: Mutex<Connection>,
}

impl SQLiteDatabaseService {
    pub fn new(db: Connection) -> Self {
        // Enable WAL mode and foreign keys
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )
        .ok();
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
                Ok(rusqlite::types::ValueRef::Real(f)) => {
                    serde_json::Number::from_f64(f)
                        .map(serde_json::Value::Number)
                        .unwrap_or(serde_json::Value::Null)
                }
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

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((n >> 18) & 63) as usize] as char);
        result.push(CHARS[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((n >> 6) & 63) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(n & 63) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
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
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            SqlValue::Text(v.to_string())
        }
    }
}

fn build_where_clause(filters: &[Filter]) -> (String, Vec<SqlValue>) {
    if filters.is_empty() {
        return (String::new(), Vec::new());
    }

    let mut clauses = Vec::new();
    let mut values = Vec::new();

    for filter in filters {
        match filter.operator {
            FilterOp::IsNull => {
                clauses.push(format!("{} IS NULL", filter.field));
            }
            FilterOp::IsNotNull => {
                clauses.push(format!("{} IS NOT NULL", filter.field));
            }
            FilterOp::In => {
                if let serde_json::Value::Array(arr) = &filter.value {
                    let placeholders: Vec<String> = arr.iter().map(|_| "?".to_string()).collect();
                    clauses.push(format!("{} IN ({})", filter.field, placeholders.join(", ")));
                    for v in arr {
                        values.push(json_to_sql_value(v));
                    }
                }
            }
            _ => {
                clauses.push(format!("{} {} ?", filter.field, filter.operator.as_sql()));
                values.push(json_to_sql_value(&filter.value));
            }
        }
    }

    (format!(" WHERE {}", clauses.join(" AND ")), values)
}

/// Auto-create a table with columns matching the provided data keys.
/// Uses TEXT type for all columns (SQLite is dynamically typed anyway).
/// The `id` column is used as the primary key.
fn ensure_table(db: &Connection, table: &str, data: &HashMap<String, serde_json::Value>) {
    let mut col_defs = Vec::new();
    for key in data.keys() {
        if key == "id" {
            col_defs.insert(0, "id TEXT PRIMARY KEY".to_string());
        } else {
            col_defs.push(format!("{} TEXT", key));
        }
    }
    if !data.contains_key("id") {
        col_defs.insert(0, "id TEXT PRIMARY KEY".to_string());
    }
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS {} ({})",
        table,
        col_defs.join(", ")
    );
    db.execute_batch(&sql).ok();

    // Also ensure any missing columns are added (for when a table exists but new fields are inserted)
    if let Ok(existing) = table_columns(db, table) {
        for key in data.keys() {
            if !existing.contains(&key.to_lowercase()) {
                let alter = format!("ALTER TABLE {} ADD COLUMN {} TEXT", table, key);
                db.execute_batch(&alter).ok();
            }
        }
    }
}

/// Get list of column names for an existing table.
fn table_columns(db: &Connection, table: &str) -> Result<Vec<String>, ()> {
    let mut stmt = db
        .prepare(&format!("PRAGMA table_info({})", table))
        .map_err(|_| ())?;
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|_| ())?
        .filter_map(|r| r.ok())
        .map(|c| c.to_lowercase())
        .collect();
    Ok(cols)
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
    if let Ok(existing) = table_columns(db, table) {
        for f in filters {
            if !existing.contains(&f.field.to_lowercase()) {
                let alter = format!("ALTER TABLE {} ADD COLUMN {} TEXT", table, f.field);
                db.execute_batch(&alter).ok();
            }
        }
        for s in sort {
            if !existing.contains(&s.field.to_lowercase()) {
                let alter = format!("ALTER TABLE {} ADD COLUMN {} TEXT", table, s.field);
                db.execute_batch(&alter).ok();
            }
        }
    }
}

fn build_order_clause(sort: &[SortField]) -> String {
    if sort.is_empty() {
        return String::new();
    }

    let parts: Vec<String> = sort
        .iter()
        .map(|s| {
            if s.desc {
                format!("{} DESC", s.field)
            } else {
                format!("{} ASC", s.field)
            }
        })
        .collect();

    format!(" ORDER BY {}", parts.join(", "))
}

impl DatabaseService for SQLiteDatabaseService {
    fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let sql = format!("SELECT * FROM {} WHERE id = ?1", collection);
        db.query_row(&sql, [id], Self::row_to_record)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => DatabaseError::NotFound,
                _ => DatabaseError::Internal(e.to_string()),
            })
    }

    fn list(&self, collection: &str, opts: &ListOptions) -> Result<RecordList, DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
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

        let (where_clause, params) = build_where_clause(&opts.filters);
        let order_clause = build_order_clause(&opts.sort);

        // Count total
        let count_sql = format!("SELECT COUNT(*) FROM {}{}", collection, where_clause);
        let count_params: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();
        let total_count: i64 = db
            .query_row(&count_sql, count_params.as_slice(), |row| row.get(0))
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        // Query records
        let mut sql = format!("SELECT * FROM {}{}{}", collection, where_clause, order_clause);

        if opts.limit > 0 {
            sql.push_str(&format!(" LIMIT {}", opts.limit));
        }
        if opts.offset > 0 {
            sql.push_str(&format!(" OFFSET {}", opts.offset));
        }

        let query_params: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();

        let mut stmt = db
            .prepare(&sql)
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        let records: Vec<Record> = stmt
            .query_map(query_params.as_slice(), Self::row_to_record)
            .map_err(|e| DatabaseError::Internal(e.to_string()))?
            .filter_map(|r| r.ok())
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
            page_size: if opts.limit > 0 { opts.limit } else { total_count },
        })
    }

    fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;

        let mut data = data;

        // Auto-generate ID if not provided
        if !data.contains_key("id") {
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
            data.insert(
                "updated_at".to_string(),
                serde_json::Value::String(now),
            );
        }

        // Auto-create table if it doesn't exist
        ensure_table(&db, collection, &data);

        let columns: Vec<&String> = data.keys().collect();
        let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("?{}", i)).collect();
        let values: Vec<SqlValue> = columns.iter().map(|k| json_to_sql_value(&data[*k])).collect();

        let col_names: Vec<&str> = columns.iter().map(|c| c.as_str()).collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            collection,
            col_names.join(", "),
            placeholders.join(", ")
        );

        let params: Vec<&dyn rusqlite::types::ToSql> =
            values.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();

        db.execute(&sql, params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        let id = match data.get("id") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };

        Ok(Record { id, data })
    }

    fn update(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;

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
            .map(|(i, k)| format!("{} = ?{}", k, i + 1))
            .collect();

        let mut values: Vec<SqlValue> = data.values().map(json_to_sql_value).collect();
        values.push(SqlValue::Text(id.to_string()));

        let sql = format!(
            "UPDATE {} SET {} WHERE id = ?{}",
            collection,
            set_clauses.join(", "),
            values.len()
        );

        let params: Vec<&dyn rusqlite::types::ToSql> =
            values.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();

        let rows = db
            .execute(&sql, params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        if rows == 0 {
            return Err(DatabaseError::NotFound);
        }

        // Fetch the updated record
        drop(db);
        self.get(collection, id)
    }

    fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let sql = format!("DELETE FROM {} WHERE id = ?1", collection);
        let rows = db
            .execute(&sql, [id])
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        if rows == 0 {
            return Err(DatabaseError::NotFound);
        }
        Ok(())
    }

    fn count(&self, collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        if !table_exists(&db, collection) {
            return Ok(0);
        }
        ensure_columns_for_query(&db, collection, filters, &[]);
        let (where_clause, params) = build_where_clause(filters);
        let sql = format!("SELECT COUNT(*) FROM {}{}", collection, where_clause);
        let query_params: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();
        db.query_row(&sql, query_params.as_slice(), |row| row.get(0))
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let (where_clause, params) = build_where_clause(filters);
        let sql = format!(
            "SELECT COALESCE(SUM({}), 0) FROM {}{}",
            field, collection, where_clause
        );
        let query_params: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();
        db.query_row(&sql, query_params.as_slice(), |row| row.get(0))
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let params: Vec<SqlValue> = args.iter().map(json_to_sql_value).collect();
        let query_params: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();

        let mut stmt = db
            .prepare(query)
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        let records: Vec<Record> = stmt
            .query_map(query_params.as_slice(), Self::row_to_record)
            .map_err(|e| DatabaseError::Internal(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(records)
    }

    fn exec_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let params: Vec<SqlValue> = args.iter().map(json_to_sql_value).collect();
        let query_params: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();

        let rows = db
            .execute(query, query_params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        Ok(rows as i64)
    }
}
