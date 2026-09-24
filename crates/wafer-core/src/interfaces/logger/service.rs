use std::fmt;

/// Service provides structured logging with levels.
///
/// Each method receives one log record from a block, delivered by the logger
/// handler:
/// - `caller` is the registered name of the block that sent it
///   (`Context::caller_id`), supplied by the runtime rather than by the
///   block, or `None` for a call with no attributable caller. An
///   implementation records it with every line, so a block cannot pass its
///   lines off as another component's.
/// - `msg` and every text in `fields` (keys and string values) arrive with
///   control characters escaped (see [`escape_log_text`]), so a record
///   renders as one line whatever the block put in it.
pub trait LoggerService: wafer_block::MaybeSend + wafer_block::MaybeSync {
    /// Emit `msg` at debug level with the given structured `fields`.
    fn debug(&self, caller: Option<&str>, msg: &str, fields: &[Field]);
    /// Emit `msg` at info level with the given structured `fields`.
    fn info(&self, caller: Option<&str>, msg: &str, fields: &[Field]);
    /// Emit `msg` at warn level with the given structured `fields`.
    fn warn(&self, caller: Option<&str>, msg: &str, fields: &[Field]);
    /// Emit `msg` at error level with the given structured `fields`.
    fn error(&self, caller: Option<&str>, msg: &str, fields: &[Field]);
}

/// Escape `text` so it renders on one line and cannot be read as more than
/// one log record: `\` becomes `\\`, newline, carriage return and tab become
/// `\n`, `\r` and `\t`, and every other control character, and the Unicode
/// line and paragraph separators (U+2028, U+2029), become `\u{..}`.
/// Escaping the backslash keeps the result unambiguous: an escaped newline
/// and a literal `\n` typed by the block read differently.
pub fn escape_log_text(text: &str) -> String {
    use fmt::Write as _;

    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || c == '\u{2028}' || c == '\u{2029}' => {
                // Writing into a `String` cannot fail.
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::escape_log_text;

    #[test]
    fn escape_log_text_leaves_one_line_and_is_unambiguous() {
        assert_eq!(
            escape_log_text("plain text, ünïcode ✓"),
            "plain text, ünïcode ✓"
        );
        assert_eq!(escape_log_text("a\nb\r\tc"), "a\\nb\\r\\tc");
        assert_eq!(
            escape_log_text("\u{1b}[31m\u{0}\u{85}"),
            "\\u{1b}[31m\\u{0}\\u{85}"
        );
        assert_eq!(
            escape_log_text("a\u{2028}b\u{2029}"),
            "a\\u{2028}b\\u{2029}"
        );
        // A literal backslash-n typed by the block does not read as an
        // escaped newline.
        assert_eq!(escape_log_text("a\\nb"), "a\\\\nb");
        assert_ne!(escape_log_text("a\\nb"), escape_log_text("a\nb"));
    }
}
