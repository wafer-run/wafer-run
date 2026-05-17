use std::fmt;

/// Service provides structured logging with levels.
pub trait LoggerService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// Emit `msg` at debug level with the given structured `fields`.
    fn debug(&self, msg: &str, fields: &[Field]);
    /// Emit `msg` at info level with the given structured `fields`.
    fn info(&self, msg: &str, fields: &[Field]);
    /// Emit `msg` at warn level with the given structured `fields`.
    fn warn(&self, msg: &str, fields: &[Field]);
    /// Emit `msg` at error level with the given structured `fields`.
    fn error(&self, msg: &str, fields: &[Field]);
}

/// Field is a key-value pair for structured log output.
#[derive(Debug, Clone)]
pub struct Field {
    /// Field name (e.g. `request_id`).
    pub key: String,
    /// Typed field value.
    pub value: FieldValue,
}

/// Typed value attached to a structured-log [`Field`].
#[derive(Debug, Clone)]
pub enum FieldValue {
    /// UTF-8 string value.
    String(String),
    /// Signed 64-bit integer value.
    Int(i64),
    /// 64-bit floating-point value.
    Float(f64),
    /// Boolean value.
    Bool(bool),
    /// Error display string (typically `error.to_string()`).
    Error(String),
    /// Arbitrary `Display`-formatted value rendered to a string.
    Any(String),
}

impl fmt::Display for FieldValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(s) => write!(f, "{s}"),
            Self::Int(i) => write!(f, "{i}"),
            Self::Float(fl) => write!(f, "{fl}"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Error(e) => write!(f, "{e}"),
            Self::Any(a) => write!(f, "{a}"),
        }
    }
}

// Helper constructors

/// Build a string-valued [`Field`].
pub fn string(key: &str, value: &str) -> Field {
    Field {
        key: key.to_string(),
        value: FieldValue::String(value.to_string()),
    }
}

/// Build an integer-valued [`Field`].
pub fn int(key: &str, value: i64) -> Field {
    Field {
        key: key.to_string(),
        value: FieldValue::Int(value),
    }
}

/// Build a float-valued [`Field`].
pub fn float(key: &str, value: f64) -> Field {
    Field {
        key: key.to_string(),
        value: FieldValue::Float(value),
    }
}

/// Build a boolean-valued [`Field`]. Named `bool_field` because `bool` is a keyword.
pub fn bool_field(key: &str, value: bool) -> Field {
    Field {
        key: key.to_string(),
        value: FieldValue::Bool(value),
    }
}

/// Build a conventional `"error"` [`Field`] from any `std::error::Error`.
pub fn err(error: &dyn std::error::Error) -> Field {
    Field {
        key: "error".to_string(),
        value: FieldValue::Error(error.to_string()),
    }
}

/// Build a [`Field`] from any `Display` value, rendered to a string.
pub fn any(key: &str, value: impl fmt::Display) -> Field {
    Field {
        key: key.to_string(),
        value: FieldValue::Any(value.to_string()),
    }
}
