use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use std::collections::HashMap;

use wafer_core::interfaces::database::service::*;

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
    // Async internals
    // -----------------------------------------------------------------

    async fn get_async(&self, collection: &str, id: &str) -> Result<Record, DatabaseError> {
        let table = sanitize_ident(collection);
        let sql = format!("SELECT * FROM {} WHERE id = $1", table);
        let row: PgRow = sqlx::query(&sql)
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map_err(|e| match e {
                sqlx::Error::RowNotFound => DatabaseError::NotFound,
                _ => DatabaseError::Internal(e.to_string()),
            })?;
        row_to_record(&row)
    }

    async fn list_async(
        &self,
        collection: &str,
        opts: &ListOptions,
    ) -> Result<RecordList, DatabaseError> {
        let table = sanitize_ident(collection);

        if !self.table_exists_async(&table).await? {
            return Ok(RecordList {
                records: Vec::new(),
                total_count: 0,
                page: 1,
                page_size: if opts.limit > 0 { opts.limit } else { 0 },
            });
        }

        // Ensure filter/sort columns exist
        self.ensure_columns_for_query(&table, &opts.filters, &opts.sort)
            .await?;

        let (where_clause, params) = build_where_clause(&opts.filters);
        let order_clause = build_order_clause(&opts.sort);

        // Count total
        let count_sql = format!("SELECT COUNT(*) FROM {}{}", table, where_clause);
        let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql);
        for p in &params {
            count_q = bind_json_value(count_q, p);
        }
        let total_count: i64 = count_q
            .fetch_one(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        // Query records
        let mut sql = format!("SELECT * FROM {}{}{}", table, where_clause, order_clause);
        if opts.limit > 0 {
            sql.push_str(&format!(" LIMIT {}", opts.limit));
        }
        if opts.offset > 0 {
            sql.push_str(&format!(" OFFSET {}", opts.offset));
        }

        let mut q = sqlx::query(&sql);
        for p in &params {
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

    async fn create_async(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        let table = sanitize_ident(collection);
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
        self.ensure_table_async(&table, &data).await?;

        let columns: Vec<String> = data.keys().cloned().collect();
        let placeholders: Vec<String> = (1..=columns.len()).map(|i| format!("${}", i)).collect();
        let values: Vec<&serde_json::Value> = columns.iter().map(|k| &data[k]).collect();

        let col_names: Vec<String> = columns.iter().map(|c| sanitize_ident(c)).collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            table,
            col_names.join(", "),
            placeholders.join(", ")
        );

        let mut q = sqlx::query(&sql);
        for v in &values {
            q = bind_json_value_query(q, v);
        }
        q.execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        let id = match data.get("id") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };

        Ok(Record { id, data })
    }

    async fn update_async(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        let table = sanitize_ident(collection);
        let mut data = data;

        // Auto-update timestamp
        if !data.contains_key("updated_at") {
            data.insert(
                "updated_at".to_string(),
                serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
            );
        }

        // Ensure any new columns exist
        self.ensure_columns_from_data(&table, &data).await?;

        let keys: Vec<String> = data.keys().cloned().collect();
        let set_clauses: Vec<String> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| format!("{} = ${}", sanitize_ident(k), i + 1))
            .collect();

        let id_param = keys.len() + 1;
        let sql = format!(
            "UPDATE {} SET {} WHERE id = ${}",
            table,
            set_clauses.join(", "),
            id_param
        );

        let mut q = sqlx::query(&sql);
        for k in &keys {
            q = bind_json_value_query(q, &data[k]);
        }
        q = q.bind(id.to_string());

        let result = q
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(DatabaseError::NotFound);
        }

        // Fetch the updated record
        self.get_async(collection, id).await
    }

    async fn delete_async(&self, collection: &str, id: &str) -> Result<(), DatabaseError> {
        let table = sanitize_ident(collection);
        let sql = format!("DELETE FROM {} WHERE id = $1", table);
        let result = sqlx::query(&sql)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(DatabaseError::NotFound);
        }
        Ok(())
    }

    async fn count_async(
        &self,
        collection: &str,
        filters: &[Filter],
    ) -> Result<i64, DatabaseError> {
        let table = sanitize_ident(collection);
        if !self.table_exists_async(&table).await? {
            return Ok(0);
        }
        self.ensure_columns_for_query(&table, filters, &[]).await?;

        let (where_clause, params) = build_where_clause(filters);
        let sql = format!("SELECT COUNT(*) FROM {}{}", table, where_clause);
        let mut q = sqlx::query_scalar::<_, i64>(&sql);
        for p in &params {
            q = bind_json_value(q, p);
        }
        q.fetch_one(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    async fn sum_async(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        let table = sanitize_ident(collection);
        let field_name = sanitize_ident(field);
        let (where_clause, params) = build_where_clause(filters);
        let sql = format!(
            "SELECT COALESCE(SUM({}), 0) FROM {}{}",
            field_name, table, where_clause
        );
        let mut q = sqlx::query_scalar::<_, f64>(&sql);
        for p in &params {
            q = bind_json_value(q, p);
        }
        q.fetch_one(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(e.to_string()))
    }

    async fn query_raw_async(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        let mut q = sqlx::query(query);
        for a in args {
            q = bind_json_value_query(q, a);
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

    async fn exec_raw_async(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        let mut q = sqlx::query(query);
        for a in args {
            q = bind_json_value_query(q, a);
        }
        let result = q
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(e.to_string()))?;
        Ok(result.rows_affected() as i64)
    }

    // -----------------------------------------------------------------
    // Schema DDL async helpers
    // -----------------------------------------------------------------

    async fn schema_ensure_table_async(&self, table: &Table) -> Result<(), DatabaseError> {
        let sql = schema_generate_create_table(table);
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(format!("create table {}: {}", table.name, e)))?;

        // Ensure indexes
        for idx in &table.indexes {
            let sql = schema_generate_create_index(&table.name, idx);
            sqlx::query(&sql)
                .execute(&self.pool)
                .await
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
                sqlx::query(&sql)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| DatabaseError::Internal(format!("create FK index: {}", e)))?;
            }
        }

        Ok(())
    }

    async fn schema_drop_table_async(&self, name: &str) -> Result<(), DatabaseError> {
        let sql = format!("DROP TABLE IF EXISTS {}", sanitize_ident(name));
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(format!("drop_table: {e}")))?;
        Ok(())
    }

    async fn schema_add_column_async(&self, table: &str, column: &Column) -> Result<(), DatabaseError> {
        let col_sql = schema_column_to_sql(column);
        let sql = format!("ALTER TABLE {} ADD COLUMN IF NOT EXISTS {}", table, col_sql);
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(format!("add_column: {e}")))?;
        Ok(())
    }

    // -----------------------------------------------------------------
    // Table/column introspection helpers
    // -----------------------------------------------------------------

    async fn table_exists_async(&self, table: &str) -> Result<bool, DatabaseError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_schema = 'public' AND table_name = $1)",
        )
        .bind(table)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| DatabaseError::Internal(format!("table_exists: {e}")))?;
        Ok(exists)
    }

    async fn get_columns(&self, table: &str) -> Result<Vec<String>, DatabaseError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT column_name FROM information_schema.columns WHERE table_schema = 'public' AND table_name = $1",
        )
        .bind(table)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| DatabaseError::Internal(format!("get_columns: {e}")))?;
        Ok(rows.into_iter().map(|(name,)| name.to_lowercase()).collect())
    }

    /// Auto-create a table with id, created_at, updated_at and any additional
    /// columns from the data map.
    async fn ensure_table_async(
        &self,
        table: &str,
        data: &HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        // Create the base table
        let create_sql = format!(
            "CREATE TABLE IF NOT EXISTS {} (id TEXT PRIMARY KEY, created_at TIMESTAMPTZ DEFAULT NOW(), updated_at TIMESTAMPTZ DEFAULT NOW())",
            table
        );
        sqlx::query(&create_sql)
            .execute(&self.pool)
            .await
            .map_err(|e| DatabaseError::Internal(format!("ensure_table: {e}")))?;

        // Add any missing columns from data
        self.ensure_columns_from_data(table, data).await
    }

    /// Add any columns that exist in `data` but not yet in the table.
    async fn ensure_columns_from_data(
        &self,
        table: &str,
        data: &HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        let existing = self.get_columns(table).await?;
        for (key, value) in data {
            if !existing.contains(&key.to_lowercase()) {
                let pg_type = pg_type_for_json_value(value);
                let alter = format!(
                    "ALTER TABLE {} ADD COLUMN IF NOT EXISTS {} {}",
                    table, sanitize_ident(key), pg_type
                );
                sqlx::query(&alter)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        DatabaseError::Internal(format!("add column {key}: {e}"))
                    })?;
            }
        }
        Ok(())
    }

    /// Ensure columns referenced in filters and sorts exist (adds them as TEXT
    /// if missing, so they default to NULL).
    async fn ensure_columns_for_query(
        &self,
        table: &str,
        filters: &[Filter],
        sort: &[SortField],
    ) -> Result<(), DatabaseError> {
        let existing = self.get_columns(table).await?;
        for f in filters {
            if !existing.contains(&f.field.to_lowercase()) {
                let alter = format!(
                    "ALTER TABLE {} ADD COLUMN IF NOT EXISTS {} TEXT",
                    table, sanitize_ident(&f.field)
                );
                sqlx::query(&alter)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        DatabaseError::Internal(format!("add filter column {}: {e}", f.field))
                    })?;
            }
        }
        for s in sort {
            if !existing.contains(&s.field.to_lowercase()) {
                let alter = format!(
                    "ALTER TABLE {} ADD COLUMN IF NOT EXISTS {} TEXT",
                    table, sanitize_ident(&s.field)
                );
                sqlx::query(&alter)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        DatabaseError::Internal(format!("add sort column {}: {e}", s.field))
                    })?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Schema DDL helpers
// ---------------------------------------------------------------------------

fn schema_data_type_to_sql(dt: DataType) -> &'static str {
    match dt {
        DataType::String | DataType::Text => "TEXT",
        DataType::Int => "INTEGER",
        DataType::Int64 => "BIGINT",
        DataType::Float => "DOUBLE PRECISION",
        DataType::Bool => "BOOLEAN",
        DataType::DateTime => "TIMESTAMPTZ",
        DataType::Json => "JSONB",
        DataType::Blob => "BYTEA",
    }
}

fn schema_default_to_sql(d: &DefaultValue) -> String {
    if d.is_null {
        return "NULL".to_string();
    }
    if d.is_raw {
        return match d.raw.as_str() {
            "CURRENT_TIMESTAMP" => "NOW()".to_string(),
            other => other.to_string(),
        };
    }
    match &d.value {
        Some(DefaultVal::String(s)) => format!("'{}'", s.replace('\'', "''")),
        Some(DefaultVal::Int(i)) => i.to_string(),
        Some(DefaultVal::Float(f)) => f.to_string(),
        Some(DefaultVal::Bool(b)) => if *b { "TRUE" } else { "FALSE" }.to_string(),
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
        sql = format!("{} SERIAL PRIMARY KEY", qname);
        if let Some(ref default) = col.default {
            sql.push_str(" DEFAULT ");
            sql.push_str(&schema_default_to_sql(default));
        }
        return sql;
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

// ---------------------------------------------------------------------------
// Trait implementation — direct async
// ---------------------------------------------------------------------------

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl DatabaseService for PostgresDatabaseService {
    async fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError> {
        self.get_async(collection, id).await
    }

    async fn list(&self, collection: &str, opts: &ListOptions) -> Result<RecordList, DatabaseError> {
        self.list_async(collection, opts).await
    }

    async fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        self.create_async(collection, data).await
    }

    async fn update(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError> {
        self.update_async(collection, id, data).await
    }

    async fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError> {
        self.delete_async(collection, id).await
    }

    async fn count(&self, collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError> {
        self.count_async(collection, filters).await
    }

    async fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError> {
        self.sum_async(collection, field, filters).await
    }

    async fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError> {
        self.query_raw_async(query, args).await
    }

    async fn exec_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError> {
        self.exec_raw_async(query, args).await
    }

    // --- Schema management ---

    async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError> {
        self.schema_ensure_table_async(table).await
    }

    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError> {
        self.table_exists_async(name).await
    }

    async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError> {
        self.schema_drop_table_async(name).await
    }

    async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError> {
        self.schema_add_column_async(table, column).await
    }
}

// ---------------------------------------------------------------------------
// Free functions: query building, type mapping, row conversion
// ---------------------------------------------------------------------------

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

/// Map a `serde_json::Value` to the appropriate PostgreSQL column type name.
fn pg_type_for_json_value(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "TEXT",
        serde_json::Value::Bool(_) => "BOOLEAN",
        serde_json::Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "BIGINT"
            } else {
                "DOUBLE PRECISION"
            }
        }
        serde_json::Value::String(_) => "TEXT",
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => "JSONB",
    }
}

/// Build a WHERE clause with `$N` placeholders from the given filters.
/// Returns `(clause_string, values)`. The clause string includes the
/// leading ` WHERE ` when there are filters.
fn build_where_clause(filters: &[Filter]) -> (String, Vec<serde_json::Value>) {
    if filters.is_empty() {
        return (String::new(), Vec::new());
    }

    let mut clauses = Vec::new();
    let mut values: Vec<serde_json::Value> = Vec::new();

    for filter in filters {
        match filter.operator {
            FilterOp::IsNull => {
                clauses.push(format!("{} IS NULL", sanitize_ident(&filter.field)));
            }
            FilterOp::IsNotNull => {
                clauses.push(format!("{} IS NOT NULL", sanitize_ident(&filter.field)));
            }
            FilterOp::In => {
                if let serde_json::Value::Array(arr) = &filter.value {
                    let placeholders: Vec<String> = arr
                        .iter()
                        .map(|v| {
                            values.push(v.clone());
                            format!("${}", values.len())
                        })
                        .collect();
                    clauses.push(format!(
                        "{} IN ({})",
                        sanitize_ident(&filter.field),
                        placeholders.join(", ")
                    ));
                }
            }
            _ => {
                values.push(filter.value.clone());
                clauses.push(format!(
                    "{} {} ${}",
                    sanitize_ident(&filter.field),
                    filter.operator.as_sql(),
                    values.len()
                ));
            }
        }
    }

    (format!(" WHERE {}", clauses.join(" AND ")), values)
}

/// Build an ORDER BY clause from sort directives.
fn build_order_clause(sort: &[SortField]) -> String {
    if sort.is_empty() {
        return String::new();
    }

    let parts: Vec<String> = sort
        .iter()
        .map(|s| {
            if s.desc {
                format!("{} DESC", sanitize_ident(&s.field))
            } else {
                format!("{} ASC", sanitize_ident(&s.field))
            }
        })
        .collect();

    format!(" ORDER BY {}", parts.join(", "))
}

/// Convert a PgRow to a Record, mapping column types to serde_json::Value.
fn row_to_record(row: &PgRow) -> Result<Record, DatabaseError> {
    use sqlx::Column as SqlxColumn;
    use sqlx::TypeInfo;

    let columns = row.columns();
    let mut data = HashMap::new();
    let mut id = String::new();

    for col in columns {
        let col_name = col.name().to_string();
        let type_name = col.type_info().name();

        let value: serde_json::Value = match type_name {
            "TEXT" | "VARCHAR" | "CHAR" | "NAME" | "BPCHAR" | "UNKNOWN" => {
                match row.try_get::<Option<String>, _>(col.ordinal()) {
                    Ok(Some(s)) => serde_json::Value::String(s),
                    Ok(None) => serde_json::Value::Null,
                    Err(_) => serde_json::Value::Null,
                }
            }
            "INT2" | "INT4" => match row.try_get::<Option<i32>, _>(col.ordinal()) {
                Ok(Some(n)) => serde_json::Value::Number(n.into()),
                Ok(None) => serde_json::Value::Null,
                Err(_) => serde_json::Value::Null,
            },
            "INT8" | "BIGINT" => match row.try_get::<Option<i64>, _>(col.ordinal()) {
                Ok(Some(n)) => serde_json::Value::Number(n.into()),
                Ok(None) => serde_json::Value::Null,
                Err(_) => serde_json::Value::Null,
            },
            "FLOAT4" => match row.try_get::<Option<f32>, _>(col.ordinal()) {
                Ok(Some(f)) => serde_json::Number::from_f64(f as f64)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null),
                Ok(None) => serde_json::Value::Null,
                Err(_) => serde_json::Value::Null,
            },
            "FLOAT8" | "DOUBLE PRECISION" | "NUMERIC" => {
                match row.try_get::<Option<f64>, _>(col.ordinal()) {
                    Ok(Some(f)) => serde_json::Number::from_f64(f)
                        .map(serde_json::Value::Number)
                        .unwrap_or(serde_json::Value::Null),
                    Ok(None) => serde_json::Value::Null,
                    Err(_) => serde_json::Value::Null,
                }
            }
            "BOOL" | "BOOLEAN" => match row.try_get::<Option<bool>, _>(col.ordinal()) {
                Ok(Some(b)) => serde_json::Value::Bool(b),
                Ok(None) => serde_json::Value::Null,
                Err(_) => serde_json::Value::Null,
            },
            "JSON" | "JSONB" => {
                match row.try_get::<Option<serde_json::Value>, _>(col.ordinal()) {
                    Ok(Some(v)) => v,
                    Ok(None) => serde_json::Value::Null,
                    Err(_) => serde_json::Value::Null,
                }
            }
            "BYTEA" => match row.try_get::<Option<Vec<u8>>, _>(col.ordinal()) {
                Ok(Some(bytes)) => serde_json::Value::String(base64_encode(&bytes)),
                Ok(None) => serde_json::Value::Null,
                Err(_) => serde_json::Value::Null,
            },
            "TIMESTAMPTZ" | "TIMESTAMP" => {
                // Try to get as string representation
                match row.try_get::<Option<String>, _>(col.ordinal()) {
                    Ok(Some(s)) => serde_json::Value::String(s),
                    Ok(None) => serde_json::Value::Null,
                    Err(_) => {
                        // Try chrono DateTime
                        match row
                            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>(col.ordinal())
                        {
                            Ok(Some(dt)) => serde_json::Value::String(dt.to_rfc3339()),
                            Ok(None) => serde_json::Value::Null,
                            Err(_) => serde_json::Value::Null,
                        }
                    }
                }
            }
            "UUID" => match row.try_get::<Option<uuid::Uuid>, _>(col.ordinal()) {
                Ok(Some(u)) => serde_json::Value::String(u.to_string()),
                Ok(None) => serde_json::Value::Null,
                Err(_) => serde_json::Value::Null,
            },
            // Fallback: try as string
            _ => match row.try_get::<Option<String>, _>(col.ordinal()) {
                Ok(Some(s)) => serde_json::Value::String(s),
                Ok(None) => serde_json::Value::Null,
                Err(_) => serde_json::Value::Null,
            },
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

/// Bind a serde_json::Value to a `sqlx::query_scalar` query.
fn bind_json_value<'q, O>(
    q: sqlx::query::QueryScalar<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments>,
    v: &'q serde_json::Value,
) -> sqlx::query::QueryScalar<'q, sqlx::Postgres, O, sqlx::postgres::PgArguments> {
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
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            q.bind(v.clone())
        }
    }
}

/// Bind a serde_json::Value to a `sqlx::query` (non-scalar).
fn bind_json_value_query<'q>(
    q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    v: &'q serde_json::Value,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
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
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            q.bind(v.clone())
        }
    }
}

/// Simple base64 encoder (no external dependency).
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_where_clause_empty() {
        let (clause, params) = build_where_clause(&[]);
        assert_eq!(clause, "");
        assert!(params.is_empty());
    }

    #[test]
    fn test_build_where_clause_equal() {
        let filters = vec![Filter {
            field: "name".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String("alice".to_string()),
        }];
        let (clause, params) = build_where_clause(&filters);
        assert_eq!(clause, " WHERE name = $1");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0], serde_json::Value::String("alice".to_string()));
    }

    #[test]
    fn test_build_where_clause_multiple() {
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
        let (clause, params) = build_where_clause(&filters);
        assert_eq!(clause, " WHERE age > $1 AND active = $2");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn test_build_where_clause_in() {
        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::In,
            value: serde_json::json!(["active", "pending", "review"]),
        }];
        let (clause, params) = build_where_clause(&filters);
        assert_eq!(clause, " WHERE status IN ($1, $2, $3)");
        assert_eq!(params.len(), 3);
        assert_eq!(
            params[0],
            serde_json::Value::String("active".to_string())
        );
        assert_eq!(
            params[1],
            serde_json::Value::String("pending".to_string())
        );
        assert_eq!(
            params[2],
            serde_json::Value::String("review".to_string())
        );
    }

    #[test]
    fn test_build_where_clause_is_null() {
        let filters = vec![Filter {
            field: "deleted_at".to_string(),
            operator: FilterOp::IsNull,
            value: serde_json::Value::Null,
        }];
        let (clause, params) = build_where_clause(&filters);
        assert_eq!(clause, " WHERE deleted_at IS NULL");
        assert!(params.is_empty());
    }

    #[test]
    fn test_build_where_clause_is_not_null() {
        let filters = vec![Filter {
            field: "email".to_string(),
            operator: FilterOp::IsNotNull,
            value: serde_json::Value::Null,
        }];
        let (clause, params) = build_where_clause(&filters);
        assert_eq!(clause, " WHERE email IS NOT NULL");
        assert!(params.is_empty());
    }

    #[test]
    fn test_build_where_clause_like() {
        let filters = vec![Filter {
            field: "name".to_string(),
            operator: FilterOp::Like,
            value: serde_json::Value::String("%alice%".to_string()),
        }];
        let (clause, params) = build_where_clause(&filters);
        assert_eq!(clause, " WHERE name LIKE $1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_build_where_clause_mixed_in_and_equal() {
        let filters = vec![
            Filter {
                field: "status".to_string(),
                operator: FilterOp::In,
                value: serde_json::json!(["a", "b"]),
            },
            Filter {
                field: "name".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!("test"),
            },
        ];
        let (clause, params) = build_where_clause(&filters);
        assert_eq!(clause, " WHERE status IN ($1, $2) AND name = $3");
        assert_eq!(params.len(), 3);
    }

    #[test]
    fn test_build_order_clause_empty() {
        let clause = build_order_clause(&[]);
        assert_eq!(clause, "");
    }

    #[test]
    fn test_build_order_clause_single_asc() {
        let sort = vec![SortField {
            field: "name".to_string(),
            desc: false,
        }];
        let clause = build_order_clause(&sort);
        assert_eq!(clause, " ORDER BY name ASC");
    }

    #[test]
    fn test_build_order_clause_multiple() {
        let sort = vec![
            SortField {
                field: "created_at".to_string(),
                desc: true,
            },
            SortField {
                field: "name".to_string(),
                desc: false,
            },
        ];
        let clause = build_order_clause(&sort);
        assert_eq!(clause, " ORDER BY created_at DESC, name ASC");
    }

    #[test]
    fn test_pg_type_for_json_value() {
        assert_eq!(pg_type_for_json_value(&serde_json::Value::Null), "TEXT");
        assert_eq!(
            pg_type_for_json_value(&serde_json::Value::Bool(true)),
            "BOOLEAN"
        );
        assert_eq!(pg_type_for_json_value(&serde_json::json!(42)), "BIGINT");
        assert_eq!(
            pg_type_for_json_value(&serde_json::json!(3.14)),
            "DOUBLE PRECISION"
        );
        assert_eq!(
            pg_type_for_json_value(&serde_json::json!("hello")),
            "TEXT"
        );
        assert_eq!(
            pg_type_for_json_value(&serde_json::json!([1, 2, 3])),
            "JSONB"
        );
        assert_eq!(
            pg_type_for_json_value(&serde_json::json!({"key": "val"})),
            "JSONB"
        );
    }

    #[test]
    fn test_sanitize_ident() {
        assert_eq!(sanitize_ident("users"), "users");
        assert_eq!(sanitize_ident("my_table"), "my_table");
        assert_eq!(sanitize_ident("table123"), "table123");
        assert_eq!(sanitize_ident("drop table;--"), "droptable");
        assert_eq!(sanitize_ident("Robert'); DROP TABLE users;--"), "RobertDROPTABLEusers");
    }

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn test_build_where_clause_not_equal() {
        let filters = vec![Filter {
            field: "status".to_string(),
            operator: FilterOp::NotEqual,
            value: serde_json::json!("deleted"),
        }];
        let (clause, params) = build_where_clause(&filters);
        assert_eq!(clause, " WHERE status != $1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn test_build_where_clause_comparison_ops() {
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
        let (clause, params) = build_where_clause(&filters);
        assert_eq!(
            clause,
            " WHERE age >= $1 AND score < $2 AND rank <= $3"
        );
        assert_eq!(params.len(), 3);
    }

    // --- Schema DDL tests (ported from wafer-run/src/schema/postgres.rs) ---

    #[test]
    fn test_schema_data_type_mapping() {
        let mappings = vec![
            (DataType::String, "TEXT"),
            (DataType::Text, "TEXT"),
            (DataType::Int, "INTEGER"),
            (DataType::Int64, "BIGINT"),
            (DataType::Float, "DOUBLE PRECISION"),
            (DataType::Bool, "BOOLEAN"),
            (DataType::DateTime, "TIMESTAMPTZ"),
            (DataType::Json, "JSONB"),
            (DataType::Blob, "BYTEA"),
        ];

        for (dt, expected) in mappings {
            assert_eq!(
                schema_data_type_to_sql(dt),
                expected,
                "DataType::{:?} should map to {}",
                dt,
                expected
            );
        }
    }

    #[test]
    fn test_schema_default_to_sql_logic() {
        // NULL default
        let d = default_null();
        assert_eq!(schema_default_to_sql(&d), "NULL");

        // Raw default (CURRENT_TIMESTAMP -> NOW())
        let d = default_now();
        assert_eq!(schema_default_to_sql(&d), "NOW()");

        // Bool defaults
        let d = default_true();
        assert_eq!(schema_default_to_sql(&d), "TRUE");

        let d = default_false();
        assert_eq!(schema_default_to_sql(&d), "FALSE");

        // Int default
        let d = default_int(42);
        assert_eq!(schema_default_to_sql(&d), "42");

        // String default
        let d = default_string("hello");
        assert_eq!(schema_default_to_sql(&d), "'hello'");
    }
}
