use rusqlite::{types::Value as SqlValue, Connection, Row};
use std::collections::HashMap;
use std::sync::Mutex;

use wafer_core::interfaces::database::service::*;

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
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
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

/// Sanitize an identifier to prevent SQL injection. Only allows
/// alphanumeric characters and underscores.
fn sanitize_ident(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// Quote an identifier for use in DDL (double-quote escaping).
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
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
        let safe_field = sanitize_ident(&filter.field);
        match filter.operator {
            FilterOp::IsNull => {
                clauses.push(format!("{} IS NULL", safe_field));
            }
            FilterOp::IsNotNull => {
                clauses.push(format!("{} IS NOT NULL", safe_field));
            }
            FilterOp::In => {
                if let serde_json::Value::Array(arr) = &filter.value {
                    let placeholders: Vec<String> = arr.iter().map(|_| "?".to_string()).collect();
                    clauses.push(format!("{} IN ({})", safe_field, placeholders.join(", ")));
                    for v in arr {
                        values.push(json_to_sql_value(v));
                    }
                }
            }
            _ => {
                clauses.push(format!("{} {} ?", safe_field, filter.operator.as_sql()));
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

fn build_order_clause(sort: &[SortField]) -> String {
    if sort.is_empty() {
        return String::new();
    }

    let parts: Vec<String> = sort
        .iter()
        .map(|s| {
            let safe_field = sanitize_ident(&s.field);
            if s.desc {
                format!("{} DESC", safe_field)
            } else {
                format!("{} ASC", safe_field)
            }
        })
        .collect();

    format!(" ORDER BY {}", parts.join(", "))
}

// ---------------------------------------------------------------------------
// Schema DDL helpers
// ---------------------------------------------------------------------------

fn schema_data_type_to_sql(dt: DataType) -> &'static str {
    match dt {
        DataType::String | DataType::Text => "TEXT",
        DataType::Int | DataType::Int64 => "INTEGER",
        DataType::Float => "REAL",
        DataType::Bool => "INTEGER",
        DataType::DateTime => "DATETIME",
        DataType::Json => "TEXT",
        DataType::Blob => "BLOB",
    }
}

fn schema_default_to_sql(d: &DefaultValue) -> String {
    if d.is_null {
        return "NULL".to_string();
    }
    if d.is_raw {
        return d.raw.clone();
    }
    match &d.value {
        Some(DefaultVal::String(s)) => format!("'{}'", s.replace('\'', "''")),
        Some(DefaultVal::Int(i)) => i.to_string(),
        Some(DefaultVal::Float(f)) => f.to_string(),
        Some(DefaultVal::Bool(b)) => if *b { "1" } else { "0" }.to_string(),
        None => "NULL".to_string(),
    }
}

fn schema_column_to_sql(col: &Column) -> String {
    let qname = quote_ident(&col.name);
    let mut sql = format!("{} {}", qname, schema_data_type_to_sql(col.data_type));

    if col.primary_key && !col.auto_increment {
        sql.push_str(" PRIMARY KEY");
    }

    if col.auto_increment {
        sql = format!("{} INTEGER PRIMARY KEY AUTOINCREMENT", qname);
    }

    if !col.nullable && !col.primary_key {
        sql.push_str(" NOT NULL");
    }

    if col.unique && !col.primary_key {
        sql.push_str(" UNIQUE");
    }

    if let Some(ref default) = col.default {
        sql.push_str(" DEFAULT ");
        sql.push_str(&schema_default_to_sql(default));
    }

    sql
}

fn schema_generate_create_table(table: &Table) -> String {
    let qtable = quote_ident(&table.name);
    let mut sql = format!("CREATE TABLE IF NOT EXISTS {} (\n", qtable);

    for (i, col) in table.columns.iter().enumerate() {
        if i > 0 {
            sql.push_str(",\n");
        }
        sql.push_str("    ");
        sql.push_str(&schema_column_to_sql(col));
    }

    // Composite primary key
    if !table.primary_key.is_empty() {
        let quoted: Vec<String> = table.primary_key.iter().map(|k| quote_ident(k)).collect();
        sql.push_str(",\n    PRIMARY KEY(");
        sql.push_str(&quoted.join(", "));
        sql.push(')');
    }

    // Composite unique constraints
    for uk in &table.unique_keys {
        let quoted: Vec<String> = uk.iter().map(|k| quote_ident(k)).collect();
        sql.push_str(",\n    UNIQUE(");
        sql.push_str(&quoted.join(", "));
        sql.push(')');
    }

    // Foreign keys
    for col in &table.columns {
        if let Some(ref refs) = col.references {
            sql.push_str(",\n    FOREIGN KEY (");
            sql.push_str(&quote_ident(&col.name));
            sql.push_str(") REFERENCES ");
            sql.push_str(&quote_ident(&refs.table));
            sql.push('(');
            sql.push_str(&quote_ident(&refs.column));
            sql.push(')');
            if !refs.on_delete.is_empty() {
                sql.push_str(" ON DELETE ");
                sql.push_str(&sanitize_ident(&refs.on_delete));
            }
            if !refs.on_update.is_empty() {
                sql.push_str(" ON UPDATE ");
                sql.push_str(&sanitize_ident(&refs.on_update));
            }
        }
    }

    sql.push_str("\n)");
    sql
}

fn schema_generate_create_index(table_name: &str, idx: &Index) -> String {
    let mut sql = String::from("CREATE ");
    if idx.unique {
        sql.push_str("UNIQUE ");
    }
    sql.push_str("INDEX IF NOT EXISTS ");

    let name = if idx.name.is_empty() {
        format!("idx_{}_{}", sanitize_ident(table_name), idx.columns.iter().map(|c| sanitize_ident(c)).collect::<Vec<_>>().join("_"))
    } else {
        sanitize_ident(&idx.name)
    };
    sql.push_str(&name);
    sql.push_str(" ON ");
    sql.push_str(&quote_ident(table_name));
    sql.push('(');
    let quoted_cols: Vec<String> = idx.columns.iter().map(|c| quote_ident(c)).collect();
    sql.push_str(&quoted_cols.join(", "));
    sql.push(')');

    sql
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl DatabaseService for SQLiteDatabaseService {
    async fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let table = sanitize_ident(collection);
        let sql = format!("SELECT * FROM {} WHERE id = ?1", table);
        db.query_row(&sql, [id], Self::row_to_record)
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => DatabaseError::NotFound,
                _ => DatabaseError::Internal(e.to_string()),
            })
    }

    async fn list(&self, collection: &str, opts: &ListOptions) -> Result<RecordList, DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
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
            page_size: if opts.limit > 0 { opts.limit } else { total_count },
        })
    }

    async fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
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
            data.insert(
                "updated_at".to_string(),
                serde_json::Value::String(now),
            );
        }

        // Auto-create table if it doesn't exist
        ensure_table(&db, &table, &data);

        let columns: Vec<&String> = data.keys().collect();
        let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("?{}", i)).collect();
        let values: Vec<SqlValue> = columns.iter().map(|k| json_to_sql_value(&data[*k])).collect();

        let safe_col_names: Vec<String> = columns.iter().map(|c| sanitize_ident(c)).collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            table,
            safe_col_names.join(", "),
            placeholders.join(", ")
        );

        let params: Vec<&dyn rusqlite::types::ToSql> =
            values.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();

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
            let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
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

            let params: Vec<&dyn rusqlite::types::ToSql> =
                values.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();

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
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
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
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let table = sanitize_ident(collection);
        if !table_exists(&db, &table) {
            return Ok(0);
        }
        ensure_columns_for_query(&db, &table, filters, &[]);
        let (where_clause, params) = build_where_clause(filters);
        let sql = format!("SELECT COUNT(*) FROM {}{}", table, where_clause);
        let query_params: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();
        db.query_row(&sql, query_params.as_slice(), |row| row.get(0))
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    async fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let table = sanitize_ident(collection);
        let safe_field = sanitize_ident(field);
        let (where_clause, params) = build_where_clause(filters);
        let sql = format!(
            "SELECT COALESCE(SUM({}), 0) FROM {}{}",
            safe_field, table, where_clause
        );
        let query_params: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();
        db.query_row(&sql, query_params.as_slice(), |row| row.get(0))
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    async fn query_raw(
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
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let params: Vec<SqlValue> = args.iter().map(json_to_sql_value).collect();
        let query_params: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();

        let rows = db
            .execute(query, query_params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        Ok(rows as i64)
    }

    async fn delete_where(&self, collection: &str, filters: &[Filter]) -> Result<(), DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let table = sanitize_ident(collection);
        if !table_exists(&db, &table) {
            return Ok(());
        }
        let (where_clause, params) = build_where_clause(filters);
        let sql = format!("DELETE FROM {}{}", table, where_clause);
        let query_params: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();
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
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
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

        let set_clauses: Vec<String> = data
            .keys()
            .enumerate()
            .map(|(i, k)| format!("{} = ?{}", sanitize_ident(k), i + 1))
            .collect();

        let mut values: Vec<SqlValue> = data.values().map(json_to_sql_value).collect();
        let (where_clause, where_params) = build_where_clause(filters);
        values.extend(where_params);

        let sql = format!("UPDATE {} SET {}{}", table, set_clauses.join(", "), where_clause);
        let query_params: Vec<&dyn rusqlite::types::ToSql> =
            values.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();
        db.execute(&sql, query_params.as_slice())
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        Ok(())
    }

    // --- Schema management ---

    async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let sql = schema_generate_create_table(table);
        db.execute_batch(&sql)
            .map_err(|e| DatabaseError::Internal(format!("create table {}: {}", table.name, e)))?;

        // Add any missing columns
        if let Ok(existing) = table_columns(&db, &table.name) {
            for col in &table.columns {
                if !existing.contains(&col.name.to_lowercase()) {
                    let alter = format!(
                        "ALTER TABLE {} ADD COLUMN {}",
                        quote_ident(&table.name),
                        schema_column_to_sql(col)
                    );
                    if let Err(e) = db.execute_batch(&alter) {
                        tracing::warn!(table = %table.name, column = %col.name, error = %e, "failed to add column");
                    }
                }
            }
        }

        // Ensure indexes
        for idx in &table.indexes {
            let sql = schema_generate_create_index(&table.name, idx);
            db.execute_batch(&sql)
                .map_err(|e| DatabaseError::Internal(format!("create index: {}", e)))?;
        }

        // Create indexes for columns with foreign keys
        for col in &table.columns {
            if col.references.is_some() {
                let idx_name = format!("idx_{}_{}", table.name, col.name);
                let sql = format!(
                    "CREATE INDEX IF NOT EXISTS {} ON {}({})",
                    idx_name, table.name, col.name
                );
                db.execute_batch(&sql)
                    .map_err(|e| DatabaseError::Internal(format!("create FK index: {}", e)))?;
            }
        }

        Ok(())
    }

    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        Ok(table_exists(&db, name))
    }

    async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        db.execute_batch(&format!("DROP TABLE IF EXISTS {}", sanitize_ident(name)))
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError> {
        let db = self.db.lock().map_err(|e| DatabaseError::Internal(e.to_string()))?;
        let sql = format!(
            "ALTER TABLE {} ADD COLUMN {}",
            sanitize_ident(table),
            schema_column_to_sql(column)
        );
        db.execute_batch(&sql)
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }
}
