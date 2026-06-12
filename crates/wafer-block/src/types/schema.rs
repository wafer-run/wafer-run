//! Database collection declarations — [`CollectionSchema`], [`FieldSchema`],
//! [`IndexSchema`], and their builders.

/// A database collection (table) declared by a block.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CollectionSchema {
    /// Collection (table) name, typically `{org}__{block}__{name}`.
    pub name: String,
    /// Field (column) definitions.
    pub fields: Vec<FieldSchema>,
    /// Indexes to be ensured on the collection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<IndexSchema>,
}

/// A field (column) in a collection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FieldSchema {
    /// Column name.
    pub name: String,
    /// Backend-portable type name (e.g. `"text"`, `"integer"`, `"json"`).
    pub field_type: String,
    /// Whether values in this column must be unique.
    #[serde(default)]
    pub unique: bool,
    /// Whether the column is nullable.
    #[serde(default)]
    pub optional: bool,
    /// Default value expression (empty = no default).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_value: String,
    /// Optional foreign-key reference, formatted as `"table.column"`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reference: String,
}

/// An index on a collection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IndexSchema {
    /// Column names included in the index, in order.
    pub fields: Vec<String>,
    /// Whether the index enforces a uniqueness constraint.
    #[serde(default)]
    pub unique: bool,
}

// ---------------------------------------------------------------------------
// Schema builder helpers
// ---------------------------------------------------------------------------

impl CollectionSchema {
    /// Start a new collection definition with the given table name.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            fields: Vec::new(),
            indexes: Vec::new(),
        }
    }

    /// Append a plain field of `field_type`.
    pub fn field(mut self, name: &str, field_type: &str) -> Self {
        self.fields.push(FieldSchema::new(name, field_type));
        self
    }

    /// Append a uniqueness-constrained field.
    pub fn field_unique(mut self, name: &str, field_type: &str) -> Self {
        self.fields
            .push(FieldSchema::new(name, field_type).set_unique());
        self
    }

    /// Append a nullable field.
    pub fn field_optional(mut self, name: &str, field_type: &str) -> Self {
        self.fields
            .push(FieldSchema::new(name, field_type).set_optional());
        self
    }

    /// Append a field with a default value.
    pub fn field_default(mut self, name: &str, field_type: &str, default: &str) -> Self {
        self.fields
            .push(FieldSchema::new(name, field_type).set_default(default));
        self
    }

    /// Append a foreign-key field referencing `reference` (`"table.column"`).
    pub fn field_ref(mut self, name: &str, field_type: &str, reference: &str) -> Self {
        self.fields
            .push(FieldSchema::new(name, field_type).set_ref(reference));
        self
    }

    /// Append a non-unique index over the given fields.
    pub fn index(mut self, fields: &[&str]) -> Self {
        self.indexes.push(IndexSchema {
            fields: fields.iter().map(|s| s.to_string()).collect(),
            unique: false,
        });
        self
    }

    /// Append a unique index over the given fields.
    pub fn unique_index(mut self, fields: &[&str]) -> Self {
        self.indexes.push(IndexSchema {
            fields: fields.iter().map(|s| s.to_string()).collect(),
            unique: true,
        });
        self
    }
}

impl FieldSchema {
    /// Create a new field with the given name and type.
    pub fn new(name: &str, field_type: &str) -> Self {
        Self {
            name: name.to_string(),
            field_type: field_type.to_string(),
            unique: false,
            optional: false,
            default_value: String::new(),
            reference: String::new(),
        }
    }

    /// Mark the field as unique.
    pub fn set_unique(mut self) -> Self {
        self.unique = true;
        self
    }
    /// Mark the field as nullable.
    pub fn set_optional(mut self) -> Self {
        self.optional = true;
        self
    }
    /// Set the field's default value expression.
    pub fn set_default(mut self, val: &str) -> Self {
        self.default_value = val.to_string();
        self
    }
    /// Set the field's foreign-key reference (`"table.column"`).
    pub fn set_ref(mut self, reference: &str) -> Self {
        self.reference = reference.to_string();
        self
    }
}
