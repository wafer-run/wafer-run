use std::fmt;

/// DataType represents a column data type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    String,
    Text,
    Int,
    Int64,
    Float,
    Bool,
    DateTime,
    Json,
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
    pub raw: String,
    pub value: Option<DefaultVal>,
    pub is_raw: bool,
    pub is_null: bool,
}

#[derive(Debug, Clone)]
pub enum DefaultVal {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

// Default helpers
pub fn default_now() -> DefaultValue {
    DefaultValue {
        raw: "CURRENT_TIMESTAMP".to_string(),
        value: None,
        is_raw: true,
        is_null: false,
    }
}

pub fn default_null() -> DefaultValue {
    DefaultValue {
        raw: String::new(),
        value: None,
        is_raw: false,
        is_null: true,
    }
}

pub fn default_zero() -> DefaultValue {
    DefaultValue {
        raw: String::new(),
        value: Some(DefaultVal::Int(0)),
        is_raw: false,
        is_null: false,
    }
}

pub fn default_empty() -> DefaultValue {
    DefaultValue {
        raw: String::new(),
        value: Some(DefaultVal::String(String::new())),
        is_raw: false,
        is_null: false,
    }
}

pub fn default_false() -> DefaultValue {
    DefaultValue {
        raw: String::new(),
        value: Some(DefaultVal::Bool(false)),
        is_raw: false,
        is_null: false,
    }
}

pub fn default_true() -> DefaultValue {
    DefaultValue {
        raw: String::new(),
        value: Some(DefaultVal::Bool(true)),
        is_raw: false,
        is_null: false,
    }
}

pub fn default_int(v: i64) -> DefaultValue {
    DefaultValue {
        raw: String::new(),
        value: Some(DefaultVal::Int(v)),
        is_raw: false,
        is_null: false,
    }
}

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
    pub table: String,
    pub column: String,
    pub on_delete: String,
    pub on_update: String,
}

/// Index defines a table index.
#[derive(Debug, Clone)]
pub struct Index {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

/// Column defines a table column.
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
    pub primary_key: bool,
    pub auto_increment: bool,
    pub unique: bool,
    pub default: Option<DefaultValue>,
    pub references: Option<Reference>,
}

impl Column {
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

    pub fn not_null(mut self) -> Self {
        self.nullable = false;
        self
    }

    pub fn null(mut self) -> Self {
        self.nullable = true;
        self
    }

    pub fn uniq(mut self) -> Self {
        self.unique = true;
        self
    }

    pub fn def(mut self, d: DefaultValue) -> Self {
        self.default = Some(d);
        self
    }

    pub fn reference(mut self, table: &str, column: &str) -> Self {
        self.references = Some(Reference {
            table: table.to_string(),
            column: column.to_string(),
            on_delete: "CASCADE".to_string(),
            on_update: String::new(),
        });
        self
    }

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

pub fn col_string(name: &str) -> Column {
    Column::new(name, DataType::String)
}

pub fn col_text(name: &str) -> Column {
    Column::new(name, DataType::Text)
}

pub fn col_int(name: &str) -> Column {
    Column::new(name, DataType::Int)
}

pub fn col_int64(name: &str) -> Column {
    Column::new(name, DataType::Int64)
}

pub fn col_float(name: &str) -> Column {
    Column::new(name, DataType::Float)
}

pub fn col_bool(name: &str) -> Column {
    Column::new(name, DataType::Bool)
}

pub fn col_datetime(name: &str) -> Column {
    Column::new(name, DataType::DateTime)
}

pub fn col_json(name: &str) -> Column {
    Column::new(name, DataType::Json)
}

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
    pub name: String,
    pub columns: Vec<Column>,
    pub indexes: Vec<Index>,
    pub primary_key: Vec<String>,
    pub unique_keys: Vec<Vec<String>>,
}

impl Table {
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
    pub tables: Vec<Table>,
}
