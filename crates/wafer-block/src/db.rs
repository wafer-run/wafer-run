//! Database query types shared between `wafer-core` (client and service
//! layers) and `wafer-sql-utils` (query builders).
//!
//! Keeping these here breaks the circular dependency that would otherwise
//! arise if `wafer-sql-utils` depended on `wafer-core` for `Filter` etc.
//! while `wafer-core` needed `wafer-sql-utils` for the `Statement` type.

use crate::{common::ErrorCode, WaferError};

/// Configures a list query (filters, sort, pagination).
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    /// Filters combined with `AND` to restrict the result set.
    pub filters: Vec<Filter>,
    /// Sort directives applied in declaration order.
    pub sort: Vec<SortField>,
    /// Maximum rows to return; `0` means backend-default.
    pub limit: i64,
    /// Number of rows to skip before returning results.
    pub offset: i64,
    /// When `true`, backends MUST skip the `SELECT COUNT(*)` query and
    /// return `RecordList.total_count = records.len() as i64`. Wrapper
    /// helpers `list_all` and `list_sorted` set this; bare `list` does
    /// not.
    pub skip_count: bool,
}

/// A single filter condition applied to a database query.
#[derive(Debug, Clone)]
pub struct Filter {
    /// Column / field name being filtered.
    pub field: String,
    /// Comparison operator.
    pub operator: FilterOp,
    /// Value to compare against (interpreted per `operator`).
    pub value: serde_json::Value,
}

/// Supported filter comparison operators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterOp {
    /// `field = value`.
    Equal,
    /// `field <> value`.
    NotEqual,
    /// `field > value`.
    GreaterThan,
    /// `field >= value`.
    GreaterEqual,
    /// `field < value`.
    LessThan,
    /// `field <= value`.
    LessEqual,
    /// `field LIKE value` (backend-specific pattern syntax).
    Like,
    /// `field IN (value…)` where `value` is a JSON array.
    In,
    /// `field IS NULL` (ignores `value`).
    IsNull,
    /// `field IS NOT NULL` (ignores `value`).
    IsNotNull,
}

impl FilterOp {
    /// Parse a wire-format filter operator string into the typed [`FilterOp`].
    ///
    /// This is the single owner of the `database@v1` wire operator grammar —
    /// the runtime database handler and test fakes both parse through here so
    /// the accepted spellings cannot drift.
    ///
    /// Returns `Err` for unknown operators so callers can surface
    /// `INVALID_ARGUMENT` rather than silently coercing unknown operators to
    /// `Equal` (which would change semantics of every malformed query into a
    /// `WHERE field = value` match — see SEC-021).
    pub fn parse_wire(op: &str) -> Result<Self, WaferError> {
        match op {
            "eq" | "=" | "equal" => Ok(Self::Equal),
            "neq" | "!=" | "not_equal" => Ok(Self::NotEqual),
            "gt" | ">" | "greater_than" => Ok(Self::GreaterThan),
            "gte" | ">=" | "greater_equal" => Ok(Self::GreaterEqual),
            "lt" | "<" | "less_than" => Ok(Self::LessThan),
            "lte" | "<=" | "less_equal" => Ok(Self::LessEqual),
            "like" => Ok(Self::Like),
            "in" => Ok(Self::In),
            "is_null" => Ok(Self::IsNull),
            "is_not_null" => Ok(Self::IsNotNull),
            other => Err(WaferError::new(
                ErrorCode::InvalidArgument,
                format!("unknown filter operator: {other:?}"),
            )),
        }
    }

    /// Render the operator as its SQL keyword form.
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

/// A sort directive for a database query.
#[derive(Debug, Clone)]
pub struct SortField {
    /// Column / field name to sort by.
    pub field: String,
    /// `true` for descending order, `false` for ascending.
    pub desc: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wire_known_ops() {
        assert!(matches!(FilterOp::parse_wire("eq"), Ok(FilterOp::Equal)));
        assert!(matches!(FilterOp::parse_wire("="), Ok(FilterOp::Equal)));
        assert!(matches!(
            FilterOp::parse_wire("neq"),
            Ok(FilterOp::NotEqual)
        ));
        assert!(matches!(FilterOp::parse_wire("like"), Ok(FilterOp::Like)));
        assert!(matches!(
            FilterOp::parse_wire("is_null"),
            Ok(FilterOp::IsNull)
        ));
    }

    #[test]
    fn parse_wire_rejects_unknown() {
        let err = FilterOp::parse_wire("bogus").expect_err("unknown op must be rejected");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(
            err.message.contains("unknown filter operator"),
            "message: {}",
            err.message
        );

        // Empty string also rejected (was previously coerced to Equal).
        let err = FilterOp::parse_wire("").expect_err("empty op must be rejected");
        assert_eq!(err.code, ErrorCode::InvalidArgument);

        // Casing matters: the wire grammar is lowercase-only.
        let err = FilterOp::parse_wire("Equal").expect_err("non-wire casing must be rejected");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }
}
