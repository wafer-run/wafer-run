use super::types::{Column, Table};

/// Adapter defines the interface for database schema operations.
pub trait SchemaAdapter: Send + Sync {
    fn ensure_table(&self, table: &Table) -> Result<(), String>;
    fn ensure_tables(&self, tables: &[Table]) -> Result<(), String>;
    fn table_exists(&self, name: &str) -> Result<bool, String>;
    fn drop_table(&self, name: &str) -> Result<(), String>;
    fn ensure_indexes(&self, table: &Table) -> Result<(), String>;
}

/// Migrator extends Adapter with migration capabilities.
pub trait SchemaMigrator: SchemaAdapter {
    fn add_column(&self, table: &str, column: &Column) -> Result<(), String>;
    fn drop_column(&self, table: &str, column: &str) -> Result<(), String>;
    fn rename_column(&self, table: &str, old_name: &str, new_name: &str) -> Result<(), String>;
    fn rename_table(&self, old_name: &str, new_name: &str) -> Result<(), String>;
}

/// SchemaProvider is implemented by extensions to declare their schema.
pub trait SchemaProvider {
    fn schema(&self) -> Vec<Table>;
}
