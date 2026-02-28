use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

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
pub trait DatabaseService: Send + Sync {
    /// Get retrieves a single record by ID from a collection.
    fn get(&self, collection: &str, id: &str) -> Result<Record, DatabaseError>;

    /// List retrieves records with optional filtering, sorting, and pagination.
    fn list(&self, collection: &str, opts: &ListOptions) -> Result<RecordList, DatabaseError>;

    /// Create inserts a new record into a collection.
    fn create(
        &self,
        collection: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError>;

    /// Update modifies an existing record by ID.
    fn update(
        &self,
        collection: &str,
        id: &str,
        data: HashMap<String, serde_json::Value>,
    ) -> Result<Record, DatabaseError>;

    /// Delete removes a record by ID.
    fn delete(&self, collection: &str, id: &str) -> Result<(), DatabaseError>;

    /// Count returns the number of records matching the filters.
    fn count(&self, collection: &str, filters: &[Filter]) -> Result<i64, DatabaseError>;

    /// Sum returns the sum of a numeric field for matching records.
    fn sum(
        &self,
        collection: &str,
        field: &str,
        filters: &[Filter],
    ) -> Result<f64, DatabaseError>;

    /// QueryRaw executes a raw SELECT query.
    fn query_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<Vec<Record>, DatabaseError>;

    /// ExecRaw executes a raw non-SELECT statement.
    fn exec_raw(
        &self,
        query: &str,
        args: &[serde_json::Value],
    ) -> Result<i64, DatabaseError>;
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

// --- Helper functions ---

/// GetByField retrieves a single record where field equals value.
pub fn get_by_field(
    db: &dyn DatabaseService,
    collection: &str,
    field: &str,
    value: serde_json::Value,
) -> Result<Record, DatabaseError> {
    let result = db.list(
        collection,
        &ListOptions {
            filters: vec![Filter {
                field: field.to_string(),
                operator: FilterOp::Equal,
                value,
            }],
            limit: 1,
            ..Default::default()
        },
    )?;
    result
        .records
        .into_iter()
        .next()
        .ok_or(DatabaseError::NotFound)
}

/// Upsert creates or updates a record based on a field match.
pub fn upsert(
    db: &dyn DatabaseService,
    collection: &str,
    field: &str,
    value: serde_json::Value,
    data: HashMap<String, serde_json::Value>,
) -> Result<Record, DatabaseError> {
    match get_by_field(db, collection, field, value) {
        Ok(existing) => db.update(collection, &existing.id, data),
        Err(DatabaseError::NotFound) => db.create(collection, data),
        Err(e) => Err(e),
    }
}

/// ListAll retrieves all records from a collection with optional filters.
pub fn list_all(
    db: &dyn DatabaseService,
    collection: &str,
    filters: Vec<Filter>,
) -> Result<Vec<Record>, DatabaseError> {
    let result = db.list(
        collection,
        &ListOptions {
            filters,
            limit: 10000,
            ..Default::default()
        },
    )?;
    Ok(result.records)
}

/// PaginatedList retrieves a page of records.
pub fn paginated_list(
    db: &dyn DatabaseService,
    collection: &str,
    page: i64,
    page_size: i64,
    filters: Vec<Filter>,
    sort: Vec<SortField>,
) -> Result<RecordList, DatabaseError> {
    let page = if page < 1 { 1 } else { page };
    let page_size = if page_size < 1 { 20 } else { page_size };
    db.list(
        collection,
        &ListOptions {
            filters,
            sort,
            limit: page_size,
            offset: (page - 1) * page_size,
        },
    )
}

/// SoftDelete sets deleted_at on a record.
pub fn soft_delete(
    db: &dyn DatabaseService,
    collection: &str,
    id: &str,
) -> Result<Record, DatabaseError> {
    let mut data = HashMap::new();
    data.insert(
        "deleted_at".to_string(),
        serde_json::Value::String("CURRENT_TIMESTAMP".to_string()),
    );
    db.update(collection, id, data)
}

/// DeleteByField deletes all records where field equals value.
pub fn delete_by_field(
    db: &dyn DatabaseService,
    collection: &str,
    field: &str,
    value: serde_json::Value,
) -> Result<(), DatabaseError> {
    let records = list_all(
        db,
        collection,
        vec![Filter {
            field: field.to_string(),
            operator: FilterOp::Equal,
            value,
        }],
    )?;
    for r in records {
        db.delete(collection, &r.id)?;
    }
    Ok(())
}

/// CountByField counts records where field equals value.
pub fn count_by_field(
    db: &dyn DatabaseService,
    collection: &str,
    field: &str,
    value: serde_json::Value,
) -> Result<i64, DatabaseError> {
    db.count(
        collection,
        &[Filter {
            field: field.to_string(),
            operator: FilterOp::Equal,
            value,
        }],
    )
}

/// DeleteByFilters deletes all records matching filters.
pub fn delete_by_filters(
    db: &dyn DatabaseService,
    collection: &str,
    filters: Vec<Filter>,
) -> Result<(), DatabaseError> {
    let records = list_all(db, collection, filters)?;
    for r in records {
        db.delete(collection, &r.id)?;
    }
    Ok(())
}

/// UpdateByFilters updates all records matching filters.
pub fn update_by_filters(
    db: &dyn DatabaseService,
    collection: &str,
    filters: Vec<Filter>,
    data: HashMap<String, serde_json::Value>,
) -> Result<(), DatabaseError> {
    let records = list_all(db, collection, filters)?;
    for r in records {
        db.update(collection, &r.id, data.clone())?;
    }
    Ok(())
}
