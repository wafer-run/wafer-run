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

/// Escape `text` so it renders on one line, as the characters it holds, and
/// cannot be read as more than one log record: `\` becomes `\\`, newline,
/// carriage return and tab become `\n`, `\r` and `\t`, and every other
/// control character, the Unicode line and paragraph separators (U+2028,
/// U+2029), the bidirectional embeddings, overrides and isolates
/// (U+202A-U+202E, U+2066-U+2069), the zero-width and directional marks
/// (U+200B-U+200F) and the byte-order mark (U+FEFF) become `\u{..}` — the
/// last three groups because they reorder or hide what a reader sees.
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
            c if c.is_control() || is_invisible_or_reordering(c) => {
                // Writing into a `String` cannot fail.
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

/// The non-control characters [`escape_log_text`] escapes: line and
/// paragraph separators, bidirectional formatting, zero-width characters and
/// the byte-order mark.
fn is_invisible_or_reordering(c: char) -> bool {
    matches!(
        c,
        '\u{2028}'
            | '\u{2029}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | '\u{200b}'..='\u{200f}'
            | '\u{feff}'
    )
}

/// `Display` adapter rendering structured fields for a text log line as
/// space-separated `key=value` pairs. A key or value that is empty or holds
/// a space, `=` or `"` is written in double quotes with its `"` escaped as
/// `\"`, so no pair can read as two, and no text can read as a field of
/// the line it sits in. Expects texts already escaped by
/// [`escape_log_text`] (as the handler delivers them), which leaves no
/// other character that could split the line.
pub struct RenderedFields<'a>(pub &'a [Field]);

impl fmt::Display for RenderedFields<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, field) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write_quoted(f, &field.key)?;
            f.write_str("=")?;
            write_quoted(f, &field.value.to_string())?;
        }
        Ok(())
    }
}

/// Write `text` for [`RenderedFields`]: bare when that is unambiguous,
/// otherwise double-quoted with `"` escaped.
fn write_quoted(f: &mut fmt::Formatter<'_>, text: &str) -> fmt::Result {
    if !text.is_empty() && !text.contains([' ', '=', '"']) {
        return f.write_str(text);
    }
    f.write_str("\"")?;
    f.write_str(&text.replace('"', "\\\""))?;
    f.write_str("\"")
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

    #[test]
    fn escape_log_text_escapes_bidi_and_zero_width_characters() {
        // `\u{202e}` would render the rest of the line right to left.
        assert_eq!(
            escape_log_text("a\u{202e}b\u{2066}c\u{2069}"),
            "a\\u{202e}b\\u{2066}c\\u{2069}"
        );
        for c in [
            '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{2067}', '\u{2068}', '\u{200b}',
            '\u{200c}', '\u{200d}', '\u{200e}', '\u{200f}', '\u{feff}',
        ] {
            let escaped = escape_log_text(&c.to_string());
            assert_eq!(escaped, format!("\\u{{{:x}}}", u32::from(c)));
        }
    }

    #[test]
    fn rendered_fields_quote_what_would_be_ambiguous() {
        use super::{any, int, string, RenderedFields};
        let fields = [
            string("plain", "value"),
            string("spaced", "hi caller=wafer-run/admin"),
            string("quoted", "say \"x\""),
            string("empty", ""),
            string("odd key", "v"),
            int("n", 3),
            any("eq", "a=b"),
        ];
        assert_eq!(
            RenderedFields(&fields).to_string(),
            r#"plain=value spaced="hi caller=wafer-run/admin" quoted="say \"x\"" empty="" "odd key"=v n=3 eq="a=b""#
        );
    }
}
