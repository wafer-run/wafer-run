use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

// Re-export schema types so consumers access them through the database module.
pub use wafer_run::schema::{
    Column, DataType, DefaultVal, DefaultValue, Index, Reference, Table,
    pk, pk_int, col_string, col_text, col_int, col_int64, col_float,
    col_bool, col_datetime, col_json, col_blob, timestamps, soft_delete as schema_soft_delete,
    default_now, default_null, default_zero, default_empty, default_false,
    default_true, default_int, default_string,
};

#[derive(Error, Debug)]
pub enum DatabaseError {
    #[error("record not found")]
    NotFound,
    #[error("database error: {0}")]
    Internal(String),
    #[error("{0}")]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

/// Service provides generic CRUD operations on collections.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait DatabaseService: Send + Sync {
    /// Get retrieves a single record by ID from a collection.
    async fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError>;

    /// List retrieves records with optional filtering, sorting, and pagination.
    async fn list(&self, collection: &str, opts: &ListOptions) -> Result<RecordList, DatabaseError>;

    /// Create inserts a new record into a collection.
    async fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError>;

    /// Update modifies an existing record by ID.
    async fn update(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError>;

    /// Delete removes a record by ID.
    async fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError>;

    /// Count returns the number of records matching the filters.
    async fn count(&self, collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError>;

    /// Sum returns the sum of a numeric field for matching records.
    async fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError>;

    /// QueryRaw executes a raw SELECT query.
    async fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError>;

    /// ExecRaw executes a raw non-SELECT statement.
    async fn exec_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError>;

    /// Bulk-delete all records matching filters in a single query.
    async fn delete_where(&self, collection: &str, filters: &[Filter]) -> Result<(), DatabaseError> {
        // Default implementation falls back to record-by-record deletion.
        // Loops until all matching records are deleted.
        loop {
            let records = self.list(
                collection,
                &ListOptions {
                    filters: filters.to_vec(),
                    limit: 10000,
                    ..Default::default()
                },
            ).await?;
            if records.records.is_empty() {
                break;
            }
            for r in records.records {
                self.delete(collection, &r.id).await?;
            }
        }
        Ok(())
    }

    /// Bulk-update all records matching filters in a single query.
    async fn update_where(
        &self,
        collection: &str,
        filters: &[Filter],
        data: HashMap<String, serde_json::Value>,
    ) -> Result<(), DatabaseError> {
        // Default implementation falls back to record-by-record updates.
        let records = self.list(
            collection,
            &ListOptions {
                filters: filters.to_vec(),
                limit: 10000,
                ..Default::default()
            },
        ).await?;

        let mut ids: Vec<String> = records.records.into_iter().map(|r| r.id).collect();
        if let Some(last_id) = ids.pop() {
            // Clone data for all but the last record.
            for id in &ids {
                self.update(collection, id, data.clone()).await?;
            }
            // Move data into the final update to avoid an extra clone.
            self.update(collection, &last_id, data).await?;
        }
        Ok(())
    }

    // --- Schema management methods ---

    /// Ensure a table exists matching the given schema definition.
    /// Creates the table if it doesn't exist and adds any missing columns.
    async fn ensure_schema_table(&self, table: &Table) -> Result<(), DatabaseError>;

    /// Ensure multiple tables exist matching the given schema definitions.
    async fn ensure_schema_tables(&self, tables: &[Table]) -> Result<(), DatabaseError> {
        for t in tables {
            self.ensure_schema_table(t).await?;
        }
        Ok(())
    }

    /// Check whether a table exists in the database.
    async fn schema_table_exists(&self, name: &str) -> Result<bool, DatabaseError>;

    /// Drop a table if it exists.
    async fn schema_drop_table(&self, name: &str) -> Result<(), DatabaseError>;

    /// Add a column to an existing table.
    async fn schema_add_column(&self, table: &str, column: &Column) -> Result<(), DatabaseError>;
}

/// Record represents a single database record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub data: HashMap<String, serde_json::Value>,
}

/// RecordList represents a paginated list of records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordList {
    pub records: Vec<Record>,
    pub total_count: i64,
    pub page: i64,
    pub page_size: i64,
}

/// ListOptions configures a List query.
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    pub filters: Vec<Filter>,
    pub sort: Vec<SortField>,
    pub limit: i64,
    pub offset: i64,
}

/// Filter represents a single filter condition.
#[derive(Debug, Clone)]
pub struct Filter {
    pub field: String,
    pub operator: FilterOp,
    pub value: serde_json::Value,
}

/// FilterOp defines supported filter operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterOp {
    Equal,
    NotEqual,
    GreaterThan,
    GreaterEqual,
    LessThan,
    LessEqual,
    Like,
    In,
    IsNull,
    IsNotNull,
}

impl FilterOp {
    pub fn as_sql(&self) -> &'static str {
        match self {
            Self::Equal => "=",
            Self::NotEqual => "!=",
            Self::GreaterThan => ">",
            Self::GreaterEqual => ">=",
            Self::LessThan => "<",
            Self::LessEqual => "<=",
            Self::Like => "LIKE",
            Self::In => "IN",
            Self::IsNull => "IS NULL",
            Self::IsNotNull => "IS NOT NULL",
        }
    }
}

/// SortField defines a sort directive.
#[derive(Debug, Clone)]
pub struct SortField {
    pub field: String,
    pub desc: bool,
}
