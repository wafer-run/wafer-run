use std::fmt;

/// DataType represents a column data type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    /// Short string column (typically VARCHAR-ish).
    String,
    /// Long-form text column with no length cap.
    Text,
    /// 32-bit signed integer.
    Int,
    /// 64-bit signed integer.
    Int64,
    /// 64-bit floating-point number.
    Float,
    /// Boolean column.
    Bool,
    /// Timestamp / datetime column.
    DateTime,
    /// JSON-encoded payload column.
    Json,
    /// Binary blob column.
    Blob,
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String => f.write_str("STRING"),
            Self::Text => f.write_str("TEXT"),
            Self::Int => f.write_str("INT"),
            Self::Int64 => f.write_str("INT64"),
            Self::Float => f.write_str("FLOAT"),
            Self::Bool => f.write_str("BOOL"),
            Self::DateTime => f.write_str("DATETIME"),
            Self::Json => f.write_str("JSON"),
            Self::Blob => f.write_str("BLOB"),
        }
    }
}

/// DefaultValue represents a column default value.
#[derive(Debug, Clone)]
pub struct DefaultValue {
    /// Raw SQL expression to splice as-is when `is_raw` is true (e.g. `CURRENT_TIMESTAMP`).
    pub raw: String,
    /// Typed literal value when `is_raw` is false and `is_null` is false.
    pub value: Option<DefaultVal>,
    /// Treat `raw` as a verbatim SQL expression instead of binding a literal.
    pub is_raw: bool,
    /// Encode `DEFAULT NULL` regardless of the other fields.
    pub is_null: bool,
}

/// Typed literal carried inside a [`DefaultValue`].
#[derive(Debug, Clone)]
pub enum DefaultVal {
    /// String literal default.
    String(String),
    /// Integer literal default.
    Int(i64),
    /// Floating-point literal default.
    Float(f64),
    /// Boolean literal default.
    Bool(bool),
}

// Default helpers
/// `DEFAULT CURRENT_TIMESTAMP` — current time at insert.
pub fn default_now() -> DefaultValue {
    DefaultValue {
        raw: "CURRENT_TIMESTAMP".to_string(),
        value: None,
        is_raw: true,
        is_null: false,
    }
}

/// `DEFAULT NULL` — explicit NULL default.
pub fn default_null() -> DefaultValue {
    DefaultValue {
        raw: String::new(),
        value: None,
        is_raw: false,
        is_null: true,
    }
}

/// `DEFAULT 0` — integer zero default.
pub fn default_zero() -> DefaultValue {
    DefaultValue {
        raw: String::new(),
        value: Some(DefaultVal::Int(0)),
        is_raw: false,
        is_null: false,
    }
}

/// `DEFAULT ''` — empty-string default.
pub fn default_empty() -> DefaultValue {
    DefaultValue {
        raw: String::new(),
        value: Some(DefaultVal::String(String::new())),
        is_raw: false,
        is_null: false,
    }
}

/// `DEFAULT false` — boolean false default.
pub fn default_false() -> DefaultValue {
    DefaultValue {
        raw: String::new(),
        value: Some(DefaultVal::Bool(false)),
        is_raw: false,
        is_null: false,
    }
}

/// `DEFAULT true` — boolean true default.
pub fn default_true() -> DefaultValue {
    DefaultValue {
        raw: String::new(),
        value: Some(DefaultVal::Bool(true)),
        is_raw: false,
        is_null: false,
    }
}

/// `DEFAULT v` — integer literal default.
pub fn default_int(v: i64) -> DefaultValue {
    DefaultValue {
        raw: String::new(),
        value: Some(DefaultVal::Int(v)),
        is_raw: false,
        is_null: false,
    }
}

/// `DEFAULT 'v'` — string literal default.
pub fn default_string(v: impl Into<String>) -> DefaultValue {
    DefaultValue {
        raw: String::new(),
        value: Some(DefaultVal::String(v.into())),
        is_raw: false,
        is_null: false,
    }
}

/// Reference defines a foreign key reference.
#[derive(Debug, Clone)]
pub struct Reference {
    /// Target table name.
    pub table: String,
    /// Target column name within `table`.
    pub column: String,
    /// `ON DELETE` action (e.g. `"CASCADE"`, `"RESTRICT"`).
    pub on_delete: String,
    /// `ON UPDATE` action (empty string omits the clause).
    pub on_update: String,
}

/// Index defines a table index.
#[derive(Debug, Clone)]
pub struct Index {
    /// Index name as it appears in the database.
    pub name: String,
    /// Ordered list of column names that make up the index key.
    pub columns: Vec<String>,
    /// `true` for a `UNIQUE` index.
    pub unique: bool,
}

/// Column defines a table column.
#[derive(Debug, Clone)]
pub struct Column {
    /// Column name.
    pub name: String,
    /// Logical column type — see [`DataType`].
    pub data_type: DataType,
    /// Whether NULL values are permitted.
    pub nullable: bool,
    /// Whether this column is part of the primary key.
    pub primary_key: bool,
    /// Whether the database assigns values automatically (e.g. AUTOINCREMENT).
    pub auto_increment: bool,
    /// Whether values must be unique across rows.
    pub unique: bool,
    /// Optional default value applied when no value is provided.
    pub default: Option<DefaultValue>,
    /// Optional foreign-key reference to another `(table, column)` pair.
    pub references: Option<Reference>,
}

impl Column {
    /// Build a non-null, non-PK column with the given name and type.
    pub fn new(name: impl Into<String>, data_type: DataType) -> Self {
        Self {
            name: name.into(),
            data_type,
            nullable: false,
            primary_key: false,
            auto_increment: false,
            unique: false,
            default: None,
            references: None,
        }
    }

    /// Mark the column as `NOT NULL`.
    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    /// Mark the column as nullable.
    pub fn null(mut self) -> Self {
        self.nullable = true;
        self
    }

    /// Mark the column as `UNIQUE`.
    pub fn uniq(mut self) -> Self {
        self.unique = true;
        self
    }

    /// Attach a default value to the column.
    pub fn def(mut self, d: DefaultValue) -> Self {
        self.default = Some(d);
        self
    }

    /// Attach a foreign-key reference with `ON DELETE CASCADE`.
    pub fn reference(mut self, table: &str, column: &str) -> Self {
        self.references = Some(Reference {
            table: table.to_string(),
            column: column.to_string(),
            on_delete: "CASCADE".to_string(),
            on_update: String::new(),
        });
        self
    }

    /// Attach a foreign-key reference with `ON DELETE RESTRICT`.
    pub fn ref_restrict(mut self, table: &str, column: &str) -> Self {
        self.references = Some(Reference {
            table: table.to_string(),
            column: column.to_string(),
            on_delete: "RESTRICT".to_string(),
            on_update: String::new(),
        });
        self
    }
}

// Column builder helpers

/// String-typed primary-key column (no auto-increment).
pub fn pk(name: &str) -> Column {
    Column {
        name: name.to_string(),
        data_type: DataType::String,
        nullable: false,
        primary_key: true,
        auto_increment: false,
        unique: false,
        default: None,
        references: None,
    }
}

/// Integer auto-increment primary-key column.
pub fn pk_int(name: &str) -> Column {
    Column {
        name: name.to_string(),
        data_type: DataType::Int,
        nullable: false,
        primary_key: true,
        auto_increment: true,
        unique: false,
        default: None,
        references: None,
    }
}

/// Helper to build a `STRING` column.
pub fn col_string(name: &str) -> Column {
    Column::new(name, DataType::String)
}

/// Helper to build a `TEXT` column.
pub fn col_text(name: &str) -> Column {
    Column::new(name, DataType::Text)
}

/// Helper to build an `INT` column.
pub fn col_int(name: &str) -> Column {
    Column::new(name, DataType::Int)
}

/// Helper to build an `INT64` column.
pub fn col_int64(name: &str) -> Column {
    Column::new(name, DataType::Int64)
}

/// Helper to build a `FLOAT` column.
pub fn col_float(name: &str) -> Column {
    Column::new(name, DataType::Float)
}

/// Helper to build a `BOOL` column.
pub fn col_bool(name: &str) -> Column {
    Column::new(name, DataType::Bool)
}

/// Helper to build a `DATETIME` column.
pub fn col_datetime(name: &str) -> Column {
    Column::new(name, DataType::DateTime)
}

/// Helper to build a `JSON` column.
pub fn col_json(name: &str) -> Column {
    Column::new(name, DataType::Json)
}

/// Helper to build a `BLOB` column.
pub fn col_blob(name: &str) -> Column {
    Column::new(name, DataType::Blob)
}

/// Timestamps returns common timestamp columns.
pub fn timestamps() -> Vec<Column> {
    vec![
        col_datetime("created_at").not_null().def(default_now()),
        col_datetime("updated_at").not_null().def(default_now()),
    ]
}

/// SoftDelete returns a deleted_at column for soft deletes.
pub fn soft_delete() -> Column {
    col_datetime("deleted_at").null()
}

/// Table defines a database table.
#[derive(Debug, Clone)]
pub struct Table {
    /// Table name.
    pub name: String,
    /// Columns in declaration order.
    pub columns: Vec<Column>,
    /// Secondary indexes (not including the primary key).
    pub indexes: Vec<Index>,
    /// Composite primary key column names (preferred over `Column::primary_key` when non-empty).
    pub primary_key: Vec<String>,
    /// Composite unique-key column groups.
    pub unique_keys: Vec<Vec<String>>,
}

impl Table {
    /// Create an empty table with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            columns: Vec::new(),
            indexes: Vec::new(),
            primary_key: Vec::new(),
            unique_keys: Vec::new(),
        }
    }
}

/// Schema is a collection of tables.
#[derive(Debug, Clone)]
pub struct Schema {
    /// Tables that make up this schema.
    pub tables: Vec<Table>,
}
