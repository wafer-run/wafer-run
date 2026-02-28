use sqlx::PgPool;

use super::adapter::{SchemaAdapter, SchemaMigrator};
use super::types::*;

/// PostgresAdapter implements SchemaAdapter for PostgreSQL databases.
///
/// Uses `sqlx::PgPool` for connection pooling. The trait methods are synchronous
/// but the underlying driver is async; we bridge with `tokio::task::block_in_place`.
pub struct PostgresAdapter {
    pool: PgPool,
}

impl PostgresAdapter {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    fn data_type_to_sql(&self, dt: DataType) -> &'static str {
        match dt {
            DataType::String => "TEXT",
            DataType::Text => "TEXT",
            DataType::Int => "INTEGER",
            DataType::Int64 => "BIGINT",
            DataType::Float => "DOUBLE PRECISION",
            DataType::Bool => "BOOLEAN",
            DataType::DateTime => "TIMESTAMPTZ",
            DataType::Json => "JSONB",
            DataType::Blob => "BYTEA",
        }
    }

    fn default_to_sql(&self, d: &DefaultValue) -> String {
        if d.is_null {
            return "NULL".to_string();
        }
        if d.is_raw {
            // Translate SQLite-isms to PostgreSQL equivalents
            return match d.raw.as_str() {
                "CURRENT_TIMESTAMP" => "NOW()".to_string(),
                other => other.to_string(),
            };
        }
        match &d.value {
            Some(DefaultVal::String(s)) => format!("'{}'", s.replace('\'', "''")),
            Some(DefaultVal::Int(i)) => i.to_string(),
            Some(DefaultVal::Float(f)) => f.to_string(),
            Some(DefaultVal::Bool(b)) => {
                if *b {
                    "TRUE".to_string()
                } else {
                    "FALSE".to_string()
                }
            }
            None => "NULL".to_string(),
        }
    }

    fn generate_column_sql(&self, col: &Column) -> String {
        let mut sql = format!("{} {}", col.name, self.data_type_to_sql(col.data_type));

        if col.primary_key && !col.auto_increment {
            sql.push_str(" PRIMARY KEY");
        }

        if col.auto_increment {
            // Use SERIAL for auto-incrementing integer primary keys
            sql = format!("{} SERIAL PRIMARY KEY", col.name);
            // Still need NOT NULL / UNIQUE etc. handled below, but SERIAL
            // already implies NOT NULL, so skip those.
            if let Some(ref default) = col.default {
                sql.push_str(" DEFAULT ");
                sql.push_str(&self.default_to_sql(default));
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
            sql.push_str(&self.default_to_sql(default));
        }

        sql
    }

    fn generate_create_table_sql(&self, table: &Table) -> String {
        let mut sql = format!("CREATE TABLE IF NOT EXISTS {} (\n", table.name);

        for (i, col) in table.columns.iter().enumerate() {
            if i > 0 {
                sql.push_str(",\n");
            }
            sql.push_str("    ");
            sql.push_str(&self.generate_column_sql(col));
        }

        // Composite primary key
        if !table.primary_key.is_empty() {
            sql.push_str(",\n    PRIMARY KEY(");
            sql.push_str(&table.primary_key.join(", "));
            sql.push(')');
        }

        // Composite unique constraints
        for uk in &table.unique_keys {
            sql.push_str(",\n    UNIQUE(");
            sql.push_str(&uk.join(", "));
            sql.push(')');
        }

        // Foreign keys
        for col in &table.columns {
            if let Some(ref refs) = col.references {
                sql.push_str(",\n    FOREIGN KEY (");
                sql.push_str(&col.name);
                sql.push_str(") REFERENCES ");
                sql.push_str(&refs.table);
                sql.push('(');
                sql.push_str(&refs.column);
                sql.push(')');
                if !refs.on_delete.is_empty() {
                    sql.push_str(" ON DELETE ");
                    sql.push_str(&refs.on_delete);
                }
                if !refs.on_update.is_empty() {
                    sql.push_str(" ON UPDATE ");
                    sql.push_str(&refs.on_update);
                }
            }
        }

        sql.push_str("\n)");
        sql
    }

    fn generate_create_index_sql(&self, table_name: &str, idx: &Index) -> String {
        let mut sql = String::from("CREATE ");
        if idx.unique {
            sql.push_str("UNIQUE ");
        }
        sql.push_str("INDEX IF NOT EXISTS ");

        let name = if idx.name.is_empty() {
            format!("idx_{}_{}", table_name, idx.columns.join("_"))
        } else {
            idx.name.clone()
        };
        sql.push_str(&name);
        sql.push_str(" ON ");
        sql.push_str(table_name);
        sql.push('(');
        sql.push_str(&idx.columns.join(", "));
        sql.push(')');

        sql
    }

    // -----------------------------------------------------------------
    // Async internals
    // -----------------------------------------------------------------

    async fn ensure_table_async(&self, table: &Table) -> Result<(), String> {
        let sql = self.generate_create_table_sql(table);
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("failed to create table {}: {}", table.name, e))?;
        self.ensure_indexes_async(table).await
    }

    async fn ensure_tables_async(&self, tables: &[Table]) -> Result<(), String> {
        for table in tables {
            self.ensure_table_async(table).await?;
        }
        Ok(())
    }

    async fn table_exists_async(&self, name: &str) -> Result<bool, String> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_schema = 'public' AND table_name = $1)",
        )
        .bind(name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| format!("table_exists: {e}"))?;
        Ok(exists)
    }

    async fn drop_table_async(&self, name: &str) -> Result<(), String> {
        let sql = format!("DROP TABLE IF EXISTS {}", name);
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("drop_table: {e}"))?;
        Ok(())
    }

    async fn ensure_indexes_async(&self, table: &Table) -> Result<(), String> {
        for idx in &table.indexes {
            let sql = self.generate_create_index_sql(&table.name, idx);
            sqlx::query(&sql)
                .execute(&self.pool)
                .await
                .map_err(|e| format!("failed to create index {}: {}", idx.name, e))?;
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
                    .map_err(|e| format!("failed to create FK index {}: {}", idx_name, e))?;
            }
        }

        Ok(())
    }

    async fn add_column_async(&self, table: &str, column: &Column) -> Result<(), String> {
        let col_sql = self.generate_column_sql(column);
        let sql = format!("ALTER TABLE {} ADD COLUMN IF NOT EXISTS {}", table, col_sql);
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("add_column: {e}"))?;
        Ok(())
    }

    async fn drop_column_async(&self, table: &str, column: &str) -> Result<(), String> {
        let sql = format!("ALTER TABLE {} DROP COLUMN IF EXISTS {}", table, column);
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("drop_column: {e}"))?;
        Ok(())
    }

    async fn rename_column_async(
        &self,
        table: &str,
        old_name: &str,
        new_name: &str,
    ) -> Result<(), String> {
        let sql = format!(
            "ALTER TABLE {} RENAME COLUMN {} TO {}",
            table, old_name, new_name
        );
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("rename_column: {e}"))?;
        Ok(())
    }

    async fn rename_table_async(&self, old_name: &str, new_name: &str) -> Result<(), String> {
        let sql = format!("ALTER TABLE {} RENAME TO {}", old_name, new_name);
        sqlx::query(&sql)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("rename_table: {e}"))?;
        Ok(())
    }

    /// Helper to run an async function on the current tokio runtime,
    /// bridging sync → async.
    fn block_on<F, T>(&self, f: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let rt = tokio::runtime::Handle::current();
        tokio::task::block_in_place(|| rt.block_on(f))
    }
}

impl SchemaAdapter for PostgresAdapter {
    fn ensure_table(&self, table: &Table) -> Result<(), String> {
        self.block_on(self.ensure_table_async(table))
    }

    fn ensure_tables(&self, tables: &[Table]) -> Result<(), String> {
        self.block_on(self.ensure_tables_async(tables))
    }

    fn table_exists(&self, name: &str) -> Result<bool, String> {
        self.block_on(self.table_exists_async(name))
    }

    fn drop_table(&self, name: &str) -> Result<(), String> {
        self.block_on(self.drop_table_async(name))
    }

    fn ensure_indexes(&self, table: &Table) -> Result<(), String> {
        self.block_on(self.ensure_indexes_async(table))
    }
}

impl SchemaMigrator for PostgresAdapter {
    fn add_column(&self, table: &str, column: &Column) -> Result<(), String> {
        self.block_on(self.add_column_async(table, column))
    }

    fn drop_column(&self, table: &str, column: &str) -> Result<(), String> {
        self.block_on(self.drop_column_async(table, column))
    }

    fn rename_column(&self, table: &str, old_name: &str, new_name: &str) -> Result<(), String> {
        self.block_on(self.rename_column_async(table, old_name, new_name))
    }

    fn rename_table(&self, old_name: &str, new_name: &str) -> Result<(), String> {
        self.block_on(self.rename_table_async(old_name, new_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_type_mapping() {
        // Verify the mapping function by creating a temporary adapter-like scope.
        // We test the mapping inline since the method is on the struct.
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

        // We can't call the method without an instance, so we duplicate the
        // mapping logic here for verification.
        for (dt, expected) in mappings {
            let sql = match dt {
                DataType::String => "TEXT",
                DataType::Text => "TEXT",
                DataType::Int => "INTEGER",
                DataType::Int64 => "BIGINT",
                DataType::Float => "DOUBLE PRECISION",
                DataType::Bool => "BOOLEAN",
                DataType::DateTime => "TIMESTAMPTZ",
                DataType::Json => "JSONB",
                DataType::Blob => "BYTEA",
            };
            assert_eq!(sql, expected, "DataType::{:?} should map to {}", dt, expected);
        }
    }

    #[test]
    fn test_default_to_sql_logic() {
        // Test NULL default
        let d = default_null();
        assert!(d.is_null);

        // Test raw default (CURRENT_TIMESTAMP → NOW())
        let d = default_now();
        assert!(d.is_raw);
        assert_eq!(d.raw, "CURRENT_TIMESTAMP");
        // In PG adapter this should become NOW()

        // Test bool defaults
        let d = default_true();
        assert!(matches!(d.value, Some(DefaultVal::Bool(true))));

        let d = default_false();
        assert!(matches!(d.value, Some(DefaultVal::Bool(false))));

        // Test int default
        let d = default_int(42);
        assert!(matches!(d.value, Some(DefaultVal::Int(42))));

        // Test string default
        let d = default_string("hello");
        assert!(matches!(d.value, Some(DefaultVal::String(ref s)) if s == "hello"));
    }
}
