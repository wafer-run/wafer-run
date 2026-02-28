use rusqlite::Connection;
use std::sync::Mutex;

use super::adapter::{SchemaAdapter, SchemaMigrator};
use super::types::*;

/// SQLiteAdapter implements SchemaAdapter for SQLite databases.
pub struct SQLiteAdapter {
    db: Mutex<Connection>,
}

impl SQLiteAdapter {
    pub fn new(db: Connection) -> Self {
        Self { db: Mutex::new(db) }
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

    fn generate_column_sql(&self, col: &Column) -> String {
        let mut sql = format!("{} {}", col.name, self.data_type_to_sql(col.data_type));

        if col.primary_key && !col.auto_increment {
            sql.push_str(" PRIMARY KEY");
        }

        if col.auto_increment {
            sql.push_str(" PRIMARY KEY AUTOINCREMENT");
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

    fn data_type_to_sql(&self, dt: DataType) -> &'static str {
        match dt {
            DataType::String => "TEXT",
            DataType::Text => "TEXT",
            DataType::Int => "INTEGER",
            DataType::Int64 => "INTEGER",
            DataType::Float => "REAL",
            DataType::Bool => "INTEGER",
            DataType::DateTime => "DATETIME",
            DataType::Json => "TEXT",
            DataType::Blob => "BLOB",
        }
    }

    fn default_to_sql(&self, d: &DefaultValue) -> String {
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
}

impl SchemaAdapter for SQLiteAdapter {
    fn ensure_table(&self, table: &Table) -> Result<(), String> {
        let db = self.db.lock().map_err(|e| e.to_string())?;
        let sql = self.generate_create_table_sql(table);
        db.execute_batch(&sql).map_err(|e| format!("failed to create table {}: {}", table.name, e))?;
        drop(db);
        self.ensure_indexes(table)
    }

    fn ensure_tables(&self, tables: &[Table]) -> Result<(), String> {
        for table in tables {
            self.ensure_table(table)?;
        }
        Ok(())
    }

    fn table_exists(&self, name: &str) -> Result<bool, String> {
        let db = self.db.lock().map_err(|e| e.to_string())?;
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [name],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(count > 0)
    }

    fn drop_table(&self, name: &str) -> Result<(), String> {
        let db = self.db.lock().map_err(|e| e.to_string())?;
        db.execute_batch(&format!("DROP TABLE IF EXISTS {}", name))
            .map_err(|e| e.to_string())
    }

    fn ensure_indexes(&self, table: &Table) -> Result<(), String> {
        let db = self.db.lock().map_err(|e| e.to_string())?;

        for idx in &table.indexes {
            let sql = self.generate_create_index_sql(&table.name, idx);
            db.execute_batch(&sql)
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
                db.execute_batch(&sql)
                    .map_err(|e| format!("failed to create FK index {}: {}", idx_name, e))?;
            }
        }

        Ok(())
    }
}

impl SchemaMigrator for SQLiteAdapter {
    fn add_column(&self, table: &str, column: &Column) -> Result<(), String> {
        let db = self.db.lock().map_err(|e| e.to_string())?;
        let sql = format!(
            "ALTER TABLE {} ADD COLUMN {}",
            table,
            self.generate_column_sql(column)
        );
        db.execute_batch(&sql).map_err(|e| e.to_string())
    }

    fn drop_column(&self, table: &str, column: &str) -> Result<(), String> {
        let db = self.db.lock().map_err(|e| e.to_string())?;
        let sql = format!("ALTER TABLE {} DROP COLUMN {}", table, column);
        db.execute_batch(&sql).map_err(|e| e.to_string())
    }

    fn rename_column(&self, table: &str, old_name: &str, new_name: &str) -> Result<(), String> {
        let db = self.db.lock().map_err(|e| e.to_string())?;
        let sql = format!(
            "ALTER TABLE {} RENAME COLUMN {} TO {}",
            table, old_name, new_name
        );
        db.execute_batch(&sql).map_err(|e| e.to_string())
    }

    fn rename_table(&self, old_name: &str, new_name: &str) -> Result<(), String> {
        let db = self.db.lock().map_err(|e| e.to_string())?;
        let sql = format!("ALTER TABLE {} RENAME TO {}", old_name, new_name);
        db.execute_batch(&sql).map_err(|e| e.to_string())
    }
}
