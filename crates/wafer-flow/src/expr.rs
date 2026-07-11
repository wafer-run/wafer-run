//! Expression parser and evaluator for `when`/`each` conditions.
//!
//! ## Precedence and associativity
//!
//! From loosest to tightest binding:
//!
//! 1. `||` (logical or)
//! 2. `&&` (logical and)
//! 3. comparison (`==`, `!=`, `>`, `<`, `>=`, `<=`) and membership (`in`,
//!    `not in`)
//!
//! `||` binds looser than `&&`, matching every mainstream language: `a ||
//! b && c` parses as `a || (b && c)`, not `(a || b) && c`. Both `&&` and
//! `||` are left-associative (`a || b || c` is `(a || b) || c`).
//!
//! Explicit grouping with `(...)` is supported and overrides the default
//! precedence, e.g. `($.a || $.b) && $.c`.

use serde_json::Value;

use crate::error::ExprError;

/// A parsed expression token.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Path(Vec<String>),
    Literal(Value),
    Compare {
        left: Box<Expr>,
        op: CompareOp,
        right: Box<Expr>,
    },
    Logical {
        left: Box<Expr>,
        op: LogicalOp,
        right: Box<Expr>,
    },
    Membership {
        value: Box<Expr>,
        op: MembershipOp,
        list: Box<Expr>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompareOp {
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
}

impl CompareOp {
    /// The source token this operator was parsed from, for error messages.
    fn as_str(self) -> &'static str {
        match self {
            CompareOp::Eq => "==",
            CompareOp::Ne => "!=",
            CompareOp::Gt => ">",
            CompareOp::Lt => "<",
            CompareOp::Ge => ">=",
            CompareOp::Le => "<=",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LogicalOp {
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MembershipOp {
    In,
    NotIn,
}

/// Validate that a string is a valid path expression ($.xxx.yyy).
pub fn parse_path(s: &str) -> Result<Vec<String>, ExprError> {
    if !s.starts_with("$.") {
        return Err(ExprError::InvalidPath(format!(
            "path must start with '$.' but got '{s}'"
        )));
    }
    let rest = &s[2..];
    if rest.is_empty() {
        return Err(ExprError::InvalidPath(
            "path must have at least one segment after '$.'".into(),
        ));
    }
    let segments: Vec<String> = rest.split('.').map(String::from).collect();
    for seg in &segments {
        if seg.is_empty() {
            return Err(ExprError::InvalidPath(format!(
                "empty segment in path '{s}'"
            )));
        }
    }
    Ok(segments)
}

/// Parse a full expression string (path, literal, comparison, logical).
pub fn parse_expr(s: &str) -> Result<Expr, ExprError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(ExprError::Parse("empty expression".into()));
    }

    // Try logical operators first (lowest precedence).
    if let Some(expr) = try_parse_logical(s)? {
        return Ok(expr);
    }

    // Try comparison operators.
    if let Some(expr) = try_parse_comparison(s)? {
        return Ok(expr);
    }

    // Try membership operators.
    if let Some(expr) = try_parse_membership(s)? {
        return Ok(expr);
    }

    // Atom: path or literal.
    parse_atom(s)
}

/// Scan `s` and invoke `f(i, bytes)` for each byte index `i` that lies
/// *outside* a string literal and *outside* a parenthesized group, where
/// `bytes` is `s.as_bytes()`. String literals are delimited by `"` or `'`;
/// a delimiter preceded by an odd number of backslashes is treated as
/// escaped (so `\"` stays inside the string but `\\"` closes it).
/// Parenthesized groups (`(...)`, which may nest) are skipped entirely so
/// operators and keywords inside an explicit `(...)` grouping never split
/// the enclosing expression — [`parse_atom`] is responsible for unwrapping
/// a fully-parenthesized atom back into its inner expression. This is the
/// single owner of the in-string/in-paren skeleton shared by the
/// operator/keyword scanners below.
fn for_each_unquoted(s: &str, mut f: impl FnMut(usize, &[u8])) {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut string_char = b'"';
    let mut paren_depth: i32 = 0;

    while i < bytes.len() {
        if in_string {
            if bytes[i] == string_char && !is_escaped(bytes, i) {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if bytes[i] == b'"' || bytes[i] == b'\'' {
            in_string = true;
            string_char = bytes[i];
            i += 1;
            continue;
        }
        if bytes[i] == b'(' {
            paren_depth += 1;
            i += 1;
            continue;
        }
        if bytes[i] == b')' {
            paren_depth = (paren_depth - 1).max(0);
            i += 1;
            continue;
        }
        if paren_depth > 0 {
            i += 1;
            continue;
        }
        f(i, bytes);
        i += 1;
    }
}

/// If `s` is a single expression fully wrapped in one matching pair of
/// parentheses — the leading `(` and trailing `)` are partners, i.e. paren
/// depth returns to zero only at the final byte — return the inner content
/// (trimmed). Returns `None` if `s` isn't parenthesized at all, or if the
/// leading `(` closes before the final character (e.g. `(a) && (b)`, where
/// the parens are two separate groups rather than one outer wrap); in that
/// case the operator scan in [`for_each_unquoted`] already found the
/// top-level split and this helper must not also unwrap it.
fn strip_outer_parens(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'(') || bytes.last() != Some(&b')') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_string = false;
    let mut string_char = b'"';
    for (idx, &b) in bytes.iter().enumerate() {
        if in_string {
            if b == string_char && !is_escaped(bytes, idx) {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' | b'\'' => {
                in_string = true;
                string_char = b;
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 && idx != bytes.len() - 1 {
                    // Closed before the end: not a single outer wrap.
                    return None;
                }
            }
            _ => {}
        }
    }
    Some(s[1..s.len() - 1].trim())
}

/// True if the byte at `i` is escaped by a preceding run of backslashes of
/// odd length (so `\"` is escaped but `\\"` is not).
fn is_escaped(bytes: &[u8], i: usize) -> bool {
    let mut backslashes = 0;
    let mut j = i;
    while j > 0 && bytes[j - 1] == b'\\' {
        backslashes += 1;
        j -= 1;
    }
    backslashes % 2 == 1
}

fn try_parse_logical(s: &str) -> Result<Option<Expr>, ExprError> {
    // `||` binds looser than `&&` (standard precedence: `a || b && c` ==
    // `a || (b && c)`, matching every mainstream language). We implement
    // this by precedence climbing at the string level: scan once for the
    // rightmost top-level `&&` AND the rightmost top-level `||`
    // independently, then split at the `||` if one exists at all — even
    // when a `&&` occurs further to the right — and only fall back to
    // splitting on `&&` when no `||` is present. Both operators are
    // left-associative: splitting at the rightmost occurrence of the
    // chosen operator and recursing on the left/right halves (which may
    // still contain earlier occurrences of the same or the other
    // operator) naturally builds a left-leaning tree at each precedence
    // level.
    let mut best_and_pos = None;
    let mut best_or_pos = None;

    for_each_unquoted(s, |i, bytes| {
        if i + 1 < bytes.len() {
            if &bytes[i..i + 2] == b"&&" {
                best_and_pos = Some(i);
            } else if &bytes[i..i + 2] == b"||" {
                best_or_pos = Some(i);
            }
        }
    });

    let split = match (best_or_pos, best_and_pos) {
        (Some(pos), _) => Some((pos, LogicalOp::Or)),
        (None, Some(pos)) => Some((pos, LogicalOp::And)),
        (None, None) => None,
    };

    if let Some((pos, op)) = split {
        let left = s[..pos].trim();
        let right = s[pos + 2..].trim();
        if left.is_empty() || right.is_empty() {
            return Err(ExprError::Parse(format!(
                "missing operand for logical operator in '{s}'"
            )));
        }
        return Ok(Some(Expr::Logical {
            left: Box::new(parse_expr(left)?),
            op,
            right: Box::new(parse_expr(right)?),
        }));
    }

    Ok(None)
}

fn try_parse_comparison(s: &str) -> Result<Option<Expr>, ExprError> {
    // Order matters: check two-char ops before single-char.
    let ops: &[(&str, CompareOp)] = &[
        ("==", CompareOp::Eq),
        ("!=", CompareOp::Ne),
        (">=", CompareOp::Ge),
        ("<=", CompareOp::Le),
        (">", CompareOp::Gt),
        ("<", CompareOp::Lt),
    ];

    for &(token, op) in ops {
        // Find the operator outside of string literals.
        if let Some(pos) = find_operator(s, token) {
            let left = s[..pos].trim();
            let right = s[pos + token.len()..].trim();
            if left.is_empty() || right.is_empty() {
                return Err(ExprError::Parse(format!(
                    "missing operand for '{token}' in '{s}'"
                )));
            }
            return Ok(Some(Expr::Compare {
                left: Box::new(parse_atom(left)?),
                op,
                right: Box::new(parse_atom(right)?),
            }));
        }
    }

    Ok(None)
}

fn try_parse_membership(s: &str) -> Result<Option<Expr>, ExprError> {
    // "not in" must be checked before "in".
    if let Some(pos) = find_keyword(s, "not in") {
        let left = s[..pos].trim();
        let right = s[pos + 6..].trim();
        if left.is_empty() || right.is_empty() {
            return Err(ExprError::Parse(format!(
                "missing operand for 'not in' in '{s}'"
            )));
        }
        return Ok(Some(Expr::Membership {
            value: Box::new(parse_atom(left)?),
            op: MembershipOp::NotIn,
            list: Box::new(parse_atom(right)?),
        }));
    }

    if let Some(pos) = find_keyword(s, "in") {
        let left = s[..pos].trim();
        let right = s[pos + 2..].trim();
        if left.is_empty() || right.is_empty() {
            return Err(ExprError::Parse(format!(
                "missing operand for 'in' in '{s}'"
            )));
        }
        return Ok(Some(Expr::Membership {
            value: Box::new(parse_atom(left)?),
            op: MembershipOp::In,
            list: Box::new(parse_atom(right)?),
        }));
    }

    Ok(None)
}

/// Find a keyword that is surrounded by whitespace (not part of an identifier).
fn find_keyword(s: &str, keyword: &str) -> Option<usize> {
    let kw_bytes = keyword.as_bytes();
    let kw_len = kw_bytes.len();
    let mut found = None;

    for_each_unquoted(s, |i, bytes| {
        if found.is_some() {
            return;
        }
        if i + kw_len <= bytes.len()
            && &bytes[i..i + kw_len] == kw_bytes
            && (i == 0 || bytes[i - 1].is_ascii_whitespace())
            && (i + kw_len >= bytes.len() || bytes[i + kw_len].is_ascii_whitespace())
        {
            found = Some(i);
        }
    });
    found
}

/// Find an operator token outside of string literals.
fn find_operator(s: &str, op: &str) -> Option<usize> {
    let op_bytes = op.as_bytes();
    let op_len = op_bytes.len();
    let mut found = None;

    for_each_unquoted(s, |i, bytes| {
        if found.is_some() {
            return;
        }
        if i + op_len <= bytes.len() && &bytes[i..i + op_len] == op_bytes {
            // For single-char ops (> or <), make sure it's not part of >= or <=, !=, ==.
            if op_len == 1
                && (op_bytes[0] == b'>' || op_bytes[0] == b'<')
                && i + 1 < bytes.len()
                && bytes[i + 1] == b'='
            {
                return;
            }
            // For single-char ops, also skip if preceded by ! or = (to avoid matching != or ==).
            if op_len == 1
                && op_bytes[0] == b'='
                && i > 0
                && (bytes[i - 1] == b'!'
                    || bytes[i - 1] == b'='
                    || bytes[i - 1] == b'>'
                    || bytes[i - 1] == b'<')
            {
                return;
            }
            found = Some(i);
        }
    });
    found
}

fn parse_atom(s: &str) -> Result<Expr, ExprError> {
    let s = s.trim();

    // Parenthesized grouping: `(expr)` recurses into the full expression
    // parser so `($.a || $.b) && $.c` groups explicitly, overriding the
    // default `||`-looser-than-`&&` precedence.
    if let Some(inner) = strip_outer_parens(s) {
        if inner.is_empty() {
            return Err(ExprError::Parse(format!(
                "empty parenthesized group in '{s}'"
            )));
        }
        return parse_expr(inner);
    }

    // Path expression.
    if s.starts_with("$.") {
        return Ok(Expr::Path(parse_path(s)?));
    }

    // String literal (double or single quotes). `s.len() >= 2` is guaranteed
    // because both the opening and closing quote are present.
    if (s.len() >= 2 && s.starts_with('"') && s.ends_with('"'))
        || (s.len() >= 2 && s.starts_with('\'') && s.ends_with('\''))
    {
        let inner = &s[1..s.len() - 1];
        return Ok(Expr::Literal(Value::String(unescape_string_literal(
            inner,
        )?)));
    }

    // Boolean.
    if s == "true" {
        return Ok(Expr::Literal(Value::Bool(true)));
    }
    if s == "false" {
        return Ok(Expr::Literal(Value::Bool(false)));
    }

    // Null.
    if s == "null" {
        return Ok(Expr::Literal(Value::Null));
    }

    // Number.
    if let Ok(n) = s.parse::<i64>() {
        return Ok(Expr::Literal(Value::Number(n.into())));
    }
    if let Ok(n) = s.parse::<f64>() {
        if let Some(num) = serde_json::Number::from_f64(n) {
            return Ok(Expr::Literal(Value::Number(num)));
        }
    }

    // JSON array literal (for membership tests).
    if s.starts_with('[') && s.ends_with(']') {
        if let Ok(val) = serde_json::from_str::<Value>(s) {
            return Ok(Expr::Literal(val));
        }
    }

    Err(ExprError::Parse(format!("unrecognized token: '{s}'")))
}

/// Process standard JSON-style escape sequences inside a string literal's
/// contents (the text between the surrounding quotes).
///
/// Recognized escapes: `\n`, `\t`, `\r`, `\"`, `\'`, `\\`, `\/`, `\b`, `\f`,
/// and `\uXXXX` (JSON-style: exactly four hex digits, no braces). A backslash
/// followed by any other
/// character is an error, so authoring typos surface instead of being silently
/// dropped or passed through.
fn unescape_string_literal(inner: &str) -> Result<String, ExprError> {
    if !inner.contains('\\') {
        return Ok(inner.to_string());
    }
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let esc = chars
            .next()
            .ok_or_else(|| ExprError::Parse("trailing backslash in string literal".into()))?;
        match esc {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{0008}'),
            'f' => out.push('\u{000C}'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'u' => {
                let mut code = 0u32;
                for _ in 0..4 {
                    let h = chars.next().ok_or_else(|| {
                        ExprError::Parse("incomplete \\u escape in string literal".into())
                    })?;
                    let digit = h.to_digit(16).ok_or_else(|| {
                        ExprError::Parse(format!("invalid hex digit '{h}' in \\u escape"))
                    })?;
                    code = code * 16 + digit;
                }
                let ch = char::from_u32(code).ok_or_else(|| {
                    ExprError::Parse(format!("invalid unicode code point U+{code:04X}"))
                })?;
                out.push(ch);
            }
            other => {
                return Err(ExprError::Parse(format!(
                    "unknown escape sequence '\\{other}' in string literal"
                )));
            }
        }
    }
    Ok(out)
}

/// Evaluate an expression against a resolver function.
pub fn eval(
    expr: &Expr,
    resolve: &dyn Fn(&[String]) -> Result<Value, ExprError>,
) -> Result<Value, ExprError> {
    match expr {
        Expr::Path(segments) => resolve(segments),
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Compare { left, op, right } => {
            let lv = eval(left, resolve)?;
            let rv = eval(right, resolve)?;
            Ok(Value::Bool(compare_values(&lv, *op, &rv)?))
        }
        Expr::Logical { left, op, right } => {
            let lv = eval(left, resolve)?;
            let lb = as_bool(&lv);
            match op {
                LogicalOp::And => {
                    if !lb {
                        return Ok(Value::Bool(false));
                    }
                    let rv = eval(right, resolve)?;
                    Ok(Value::Bool(as_bool(&rv)))
                }
                LogicalOp::Or => {
                    if lb {
                        return Ok(Value::Bool(true));
                    }
                    let rv = eval(right, resolve)?;
                    Ok(Value::Bool(as_bool(&rv)))
                }
            }
        }
        Expr::Membership { value, op, list } => {
            let v = eval(value, resolve)?;
            let l = eval(list, resolve)?;
            let arr = l.as_array().ok_or_else(|| {
                ExprError::TypeError("membership test requires an array on the right side".into())
            })?;
            let contains = arr.contains(&v);
            Ok(Value::Bool(match op {
                MembershipOp::In => contains,
                MembershipOp::NotIn => !contains,
            }))
        }
    }
}

fn compare_values(left: &Value, op: CompareOp, right: &Value) -> Result<bool, ExprError> {
    match op {
        // Equality works for any value types (structural comparison).
        CompareOp::Eq => Ok(left == right),
        CompareOp::Ne => Ok(left != right),
        // Ordered comparisons require both operands to be numeric. A non-numeric
        // operand (e.g. comparing string fields with `>`) is an authoring error
        // and must surface rather than silently evaluating to `false`.
        CompareOp::Gt | CompareOp::Lt | CompareOp::Ge | CompareOp::Le => {
            match (as_f64(left), as_f64(right)) {
                (Some(l), Some(r)) => Ok(match op {
                    CompareOp::Gt => l > r,
                    CompareOp::Lt => l < r,
                    CompareOp::Ge => l >= r,
                    CompareOp::Le => l <= r,
                    _ => unreachable!(),
                }),
                _ => Err(ExprError::TypeError(format!(
                    "ordered comparison '{}' requires numeric operands, got {} and {}",
                    op.as_str(),
                    type_name(left),
                    type_name(right),
                ))),
            }
        }
    }
}

/// A human-readable JSON type name, used in [`ExprError::TypeError`] messages.
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn as_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn dummy_resolve(segments: &[String]) -> Result<Value, ExprError> {
        match segments.first().map(String::as_str) {
            Some("input") => match segments.get(1).map(String::as_str) {
                Some("email") => Ok(json!("alice@example.com")),
                Some("age") => Ok(json!(25)),
                Some("active") => Ok(json!(true)),
                Some("roles") => Ok(json!(["admin", "user"])),
                _ => Err(ExprError::UnresolvedReference(segments.join("."))),
            },
            Some("step1") => match segments.get(1).map(String::as_str) {
                Some("count") => Ok(json!(10)),
                Some("match") => Ok(json!(false)),
                _ => Err(ExprError::UnresolvedReference(segments.join("."))),
            },
            _ => Err(ExprError::UnresolvedReference(segments.join("."))),
        }
    }

    #[test]
    fn parse_path_expression() {
        let segments = parse_path("$.input.email").unwrap();
        assert_eq!(segments, vec!["input", "email"]);
    }

    #[test]
    fn parse_and_eval_comparison() {
        let expr = parse_expr("$.step1.count > 5").unwrap();
        let result = eval(&expr, &dummy_resolve).unwrap();
        assert_eq!(result, json!(true));
    }

    #[test]
    fn parse_and_eval_equality() {
        let expr = parse_expr("$.step1.match == false").unwrap();
        let result = eval(&expr, &dummy_resolve).unwrap();
        assert_eq!(result, json!(true));
    }

    #[test]
    fn parse_and_eval_string_comparison() {
        let expr = parse_expr("$.input.email == \"alice@example.com\"").unwrap();
        let result = eval(&expr, &dummy_resolve).unwrap();
        assert_eq!(result, json!(true));
    }

    #[test]
    fn parse_and_eval_logical_and() {
        let expr = parse_expr("$.input.active == true && $.step1.count > 5").unwrap();
        let result = eval(&expr, &dummy_resolve).unwrap();
        assert_eq!(result, json!(true));
    }

    #[test]
    fn parse_and_eval_logical_or() {
        let expr = parse_expr("$.step1.match == true || $.step1.count > 5").unwrap();
        let result = eval(&expr, &dummy_resolve).unwrap();
        assert_eq!(result, json!(true));
    }

    #[test]
    fn parse_and_eval_membership_in() {
        let expr = parse_expr("\"admin\" in $.input.roles").unwrap();
        let result = eval(&expr, &dummy_resolve).unwrap();
        assert_eq!(result, json!(true));
    }

    #[test]
    fn parse_and_eval_membership_not_in() {
        let expr = parse_expr("\"guest\" not in $.input.roles").unwrap();
        let result = eval(&expr, &dummy_resolve).unwrap();
        assert_eq!(result, json!(true));
    }

    #[test]
    fn parse_and_eval_membership_in_with_tab_boundaries() {
        // The `in` keyword surrounded by tabs (not just ASCII spaces) must
        // still be recognized — find_keyword's doc promises "surrounded by
        // whitespace", and operands are trimmed before parsing.
        let expr = parse_expr("\"admin\"\tin\t$.input.roles").unwrap();
        let result = eval(&expr, &dummy_resolve).unwrap();
        assert_eq!(result, json!(true));
    }

    #[test]
    fn parse_literal_number() {
        let expr = parse_expr("42").unwrap();
        assert_eq!(expr, Expr::Literal(json!(42)));
    }

    #[test]
    fn parse_literal_null() {
        let expr = parse_expr("null").unwrap();
        assert_eq!(expr, Expr::Literal(Value::Null));
    }

    #[test]
    fn invalid_path() {
        assert!(parse_path("input.email").is_err());
        assert!(parse_path("$.").is_err());
        assert!(parse_path("$..foo").is_err());
    }

    #[test]
    fn string_literal_processes_escapes() {
        // \n must become a real newline, not a literal backslash-n.
        let expr = parse_expr(r#""a\nb""#).unwrap();
        assert_eq!(expr, Expr::Literal(json!("a\nb")));

        let expr = parse_expr(r#""tab\there""#).unwrap();
        assert_eq!(expr, Expr::Literal(json!("tab\there")));

        // Escaped quote inside a double-quoted string.
        let expr = parse_expr(r#""say \"hi\"""#).unwrap();
        assert_eq!(expr, Expr::Literal(json!(r#"say "hi""#)));

        // Escaped backslash collapses to a single backslash.
        let expr = parse_expr(r#""c:\\tmp""#).unwrap();
        assert_eq!(expr, Expr::Literal(json!(r"c:\tmp")));

        // \u escape: U+0041 is 'A'. Build the input as `"A"` (with the
        // backslash as data, not a Rust escape) via concatenation so the test
        // source is unambiguous.
        let u_input = format!("\"{}u0041\"", '\\');
        let expr = parse_expr(&u_input).unwrap();
        assert_eq!(expr, Expr::Literal(json!("A")));
    }

    #[test]
    fn string_literal_without_escapes_is_verbatim() {
        let expr = parse_expr(r#""plain text""#).unwrap();
        assert_eq!(expr, Expr::Literal(json!("plain text")));
    }

    #[test]
    fn string_literal_rejects_unknown_escape() {
        assert!(parse_expr(r#""bad \q escape""#).is_err());
    }

    #[test]
    fn ordered_comparison_of_strings_is_type_error() {
        // `$.a.name > $.b.name` (string operands) must surface a TypeError
        // rather than silently evaluating the branch as not-taken.
        let resolve = |segments: &[String]| -> Result<Value, ExprError> {
            match segments.first().map(String::as_str) {
                Some("a") => Ok(json!("alpha")),
                Some("b") => Ok(json!("beta")),
                _ => Err(ExprError::UnresolvedReference(segments.join("."))),
            }
        };
        let expr = parse_expr("$.a > $.b").unwrap();
        let err = eval(&expr, &resolve).unwrap_err();
        assert!(
            matches!(err, ExprError::TypeError(_)),
            "expected TypeError, got {err:?}"
        );
    }

    #[test]
    fn numeric_ordered_comparison_still_works() {
        // Both `>` directions on numeric operands keep evaluating.
        let expr = parse_expr("$.step1.count > 5").unwrap();
        assert_eq!(eval(&expr, &dummy_resolve).unwrap(), json!(true));
        let expr = parse_expr("$.step1.count < 5").unwrap();
        assert_eq!(eval(&expr, &dummy_resolve).unwrap(), json!(false));
        let expr = parse_expr("$.step1.count >= 10").unwrap();
        assert_eq!(eval(&expr, &dummy_resolve).unwrap(), json!(true));
    }

    #[test]
    fn string_equality_does_not_error() {
        // Equality (==/!=) of non-numeric operands must keep working, never error.
        let expr = parse_expr("$.input.email == \"alice@example.com\"").unwrap();
        assert_eq!(eval(&expr, &dummy_resolve).unwrap(), json!(true));
        let expr = parse_expr("$.input.email != \"bob@example.com\"").unwrap();
        assert_eq!(eval(&expr, &dummy_resolve).unwrap(), json!(true));
    }

    #[test]
    fn escaped_quote_does_not_break_comparison_split() {
        // The comparison split must treat the escaped inner quotes as content,
        // and the resulting literal must have escapes processed.
        let expr = parse_expr(r#"$.input.email == "a\"b""#).unwrap();
        match expr {
            Expr::Compare { right, .. } => {
                assert_eq!(*right, Expr::Literal(json!(r#"a"b"#)));
            }
            other => panic!("expected comparison, got {other:?}"),
        }
    }

    #[test]
    fn or_binds_looser_than_and() {
        // `a || b && c` must parse as `a || (b && c)`, matching every
        // mainstream language's precedence. With a=1 (so `$.a == 1` is
        // true), b=0, c=0 (so both `$.b == 1` and `$.c == 1` are false),
        // the correct result is `true` because `||` short-circuits on its
        // left operand. The equal-precedence bug instead splits at the
        // rightmost operator of either kind (here `&&`), producing
        // `(a || b) && c` = `true && false` = `false`.
        let ctx = json!({ "a": 1, "b": 0, "c": 0 });
        let resolve = |segments: &[String]| -> Result<Value, ExprError> {
            ctx.get(segments.first().map(String::as_str).unwrap_or_default())
                .cloned()
                .ok_or_else(|| ExprError::UnresolvedReference(segments.join(".")))
        };
        let expr = parse_expr("$.a == 1 || $.b == 1 && $.c == 1").unwrap();
        assert_eq!(eval(&expr, &resolve).unwrap(), json!(true));
    }

    #[test]
    fn and_binds_tighter_than_or_in_middle() {
        // `a && b || c` must parse as `(a && b) || c`. With a=0 (false),
        // b=1 (true), c=1 (true): `(a && b) || c` = `false || true` =
        // `true`, while the wrong grouping `a && (b || c)` would give
        // `false && true` = `false`.
        let ctx = json!({ "a": 0, "b": 1, "c": 1 });
        let resolve = |segments: &[String]| -> Result<Value, ExprError> {
            ctx.get(segments.first().map(String::as_str).unwrap_or_default())
                .cloned()
                .ok_or_else(|| ExprError::UnresolvedReference(segments.join(".")))
        };
        let expr = parse_expr("$.a == 1 && $.b == 1 || $.c == 1").unwrap();
        assert_eq!(eval(&expr, &resolve).unwrap(), json!(true));
    }

    #[test]
    fn parenthesized_grouping_overrides_precedence() {
        // Explicit parens must override the default `||`-looser-than-`&&`
        // precedence. With a=1, b=0, c=0 (the same context as
        // `or_binds_looser_than_and`), the unparenthesized expression
        // `a || b && c` is `true` (a || (b && c)). Wrapping the `||` in
        // parens forces `(a || b) && c` instead, which evaluates to
        // `false` — the opposite answer, proving the grouping took effect.
        let ctx = json!({ "a": 1, "b": 0, "c": 0 });
        let resolve = |segments: &[String]| -> Result<Value, ExprError> {
            ctx.get(segments.first().map(String::as_str).unwrap_or_default())
                .cloned()
                .ok_or_else(|| ExprError::UnresolvedReference(segments.join(".")))
        };
        let expr = parse_expr("($.a == 1 || $.b == 1) && $.c == 1").unwrap();
        assert_eq!(eval(&expr, &resolve).unwrap(), json!(false));
    }

    #[test]
    fn redundant_nested_parens_unwrap_to_atom() {
        let expr = parse_expr("(($.a))").unwrap();
        assert_eq!(expr, Expr::Path(vec!["a".to_string()]));
    }

    #[test]
    fn unbalanced_parens_are_a_parse_error() {
        assert!(parse_expr("($.a || $.b").is_err());
    }

    #[test]
    fn separate_paren_groups_still_split_at_top_level_operator() {
        // `(a) && (b)` has two independent groups, not one outer wrap;
        // strip_outer_parens must not collapse them into a single atom.
        let ctx = json!({ "a": 1, "b": 1 });
        let resolve = |segments: &[String]| -> Result<Value, ExprError> {
            ctx.get(segments.first().map(String::as_str).unwrap_or_default())
                .cloned()
                .ok_or_else(|| ExprError::UnresolvedReference(segments.join(".")))
        };
        let expr = parse_expr("($.a == 1) && ($.b == 1)").unwrap();
        assert_eq!(eval(&expr, &resolve).unwrap(), json!(true));
    }

    #[test]
    fn escaped_backslash_before_quote_closes_string() {
        // The first literal ends with an escaped backslash (`\\`) immediately
        // before its closing quote. A naive `bytes[i-1] != '\\'` escape check
        // mistakes that quote for an escaped one and never leaves the string,
        // swallowing the `&&` that follows. The parity-based scanner sees the
        // even backslash run, closes the string, and splits on `&&`.
        let expr = parse_expr(r#"$.a == "c:\\" && $.b == "x""#).unwrap();
        match expr {
            Expr::Logical { left, op, right } => {
                assert_eq!(op, LogicalOp::And);
                match *left {
                    Expr::Compare { right: lit, .. } => {
                        // `c:\\` unescapes to a single trailing backslash.
                        assert_eq!(*lit, Expr::Literal(json!(r"c:\")));
                    }
                    other => panic!("expected comparison on left, got {other:?}"),
                }
                match *right {
                    Expr::Compare { right: lit, .. } => {
                        assert_eq!(*lit, Expr::Literal(json!("x")));
                    }
                    other => panic!("expected comparison on right, got {other:?}"),
                }
            }
            other => panic!("expected logical expression, got {other:?}"),
        }
    }
}
