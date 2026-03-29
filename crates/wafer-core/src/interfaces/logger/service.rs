use std::fmt;

/// Service provides structured logging with levels.
pub trait LoggerService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    fn debug(&self, msg: &str, fields: &[Field]);
    fn info(&self, msg: &str, fields: &[Field]);
    fn warn(&self, msg: &str, fields: &[Field]);
    fn error(&self, msg: &str, fields: &[Field]);
}

/// Field is a key-value pair for structured log output.
#[derive(Debug, Clone)]
pub struct Field {
    pub key: String,
    pub value: FieldValue,
}

#[derive(Debug, Clone)]
pub enum FieldValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Error(String),
    Any(String),
}

impl fmt::Display for FieldValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(s) => write!(f, "{}", s),
            Self::Int(i) => write!(f, "{}", i),
            Self::Float(fl) => write!(f, "{}", fl),
            Self::Bool(b) => write!(f, "{}", b),
            Self::Error(e) => write!(f, "{}", e),
            Self::Any(a) => write!(f, "{}", a),
        }
    }
}

// Helper constructors

pub fn string(key: &str, value: &str) -> Field {
    Field {
        key: key.to_string(),
        value: FieldValue::String(value.to_string()),
    }
}

pub fn int(key: &str, value: i64) -> Field {
    Field {
        key: key.to_string(),
        value: FieldValue::Int(value),
    }
}

pub fn float(key: &str, value: f64) -> Field {
    Field {
        key: key.to_string(),
        value: FieldValue::Float(value),
    }
}

pub fn bool_field(key: &str, value: bool) -> Field {
    Field {
        key: key.to_string(),
        value: FieldValue::Bool(value),
    }
}

pub fn err(error: &dyn std::error::Error) -> Field {
    Field {
        key: "error".to_string(),
        value: FieldValue::Error(error.to_string()),
    }
}

pub fn any(key: &str, value: impl fmt::Display) -> Field {
    Field {
        key: key.to_string(),
        value: FieldValue::Any(value.to_string()),
    }
}
