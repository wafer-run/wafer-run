//! Database query types shared between `wafer-core` (client and service
//! layers) and `wafer-sql-utils` (query builders).
//!
//! Keeping these here breaks the circular dependency that would otherwise
//! arise if `wafer-sql-utils` depended on `wafer-core` for `Filter` etc.
//! while `wafer-core` needed `wafer-sql-utils` for the `Statement` type.

use crate::{common::ErrorCode, WaferError};

/// Longest table or column name the database layer accepts, in bytes.
/// PostgreSQL keeps only the first 63 bytes of an identifier and drops the
/// rest without an error, so two longer names sharing those 63 bytes would
/// name the same table. SQLite has no such limit; the one rule holds on both.
pub const MAX_IDENT_LEN: usize = 63;

/// Whether `name` is a table or column name the database layer accepts:
/// non-empty, at most [`MAX_IDENT_LEN`] bytes, ASCII lowercase letters,
/// digits and `_` only. The one rule behind the database handler's wire
/// check and `wafer_sql_utils::ident::validate_ident`. Lowercase only, so a
/// table has one spelling on every backend: SQLite folds the case of a name,
/// PostgreSQL keeps it for a quoted one.
#[must_use]
pub fn is_plain_ident(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_IDENT_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Configures a list query (filters, sort, pagination).
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    /// Filters combined with `AND` to restrict the result set.
    pub filters: Vec<Filter>,
    /// Sort directives applied in declaration order.
    pub sort: Vec<SortField>,
    /// Maximum rows to return, at least 1; `None` returns every matching row.
    pub limit: Option<u32>,
    /// Number of rows to skip before returning results. A positive offset
    /// needs a `limit`.
    pub offset: i64,
    /// When `true`, backends MUST skip the `SELECT COUNT(*)` query and
    /// return `RecordList.total_count = records.len() as i64`. Wrapper
    /// helpers `list_all` and `list_sorted` set this; bare `list` does
    /// not.
    pub skip_count: bool,
    /// Optional predicate **tree** (AND/OR groups). When `Some`, backends use
    /// it in preference to `filters` (which stays for the flat-AND fast path
    /// and back-compat). `None` = use `filters`.
    pub filter_tree: Option<Vec<FilterTree>>,
    /// Optional column projection. `None` selects every column
    /// (`SELECT *`); `Some(cols)` selects exactly `cols` (`SELECT
    /// {cols}`). An explicit empty `Vec` is rejected by the handler before
    /// it reaches here — see `database::handler`'s `DATABASE_LIST` arm.
    pub columns: Option<Vec<String>>,
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

/// A comparison between two columns of the same row: `field <operator>
/// column`.
///
/// The builder-input analogue of a wire `FilterDef` that names a `column`
/// instead of a `value`. It is a [`FilterTree`] leaf only — never a flat
/// [`Filter`] — so the ops that take flat filters cannot receive one.
#[derive(Debug, Clone)]
pub struct ColumnFilter {
    /// Left-hand column.
    pub field: String,
    /// Comparison operator.
    pub operator: ColumnCompareOp,
    /// Right-hand column.
    pub column: String,
}

/// The operators a [`ColumnFilter`] supports: the equality and ordering
/// subset of [`FilterOp`]. `LIKE`, `IN` and the null tests take a value, not a
/// second column, so they have no column form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnCompareOp {
    /// `field = column`.
    Equal,
    /// `field <> column`.
    NotEqual,
    /// `field > column`.
    GreaterThan,
    /// `field >= column`.
    GreaterEqual,
    /// `field < column`.
    LessThan,
    /// `field <= column`.
    LessEqual,
}

impl ColumnCompareOp {
    /// The column form of `op`, or `None` for an operator that has none
    /// (`Like`, `In`, `IsNull`, `IsNotNull`).
    #[must_use]
    pub fn from_filter_op(op: &FilterOp) -> Option<Self> {
        match op {
            FilterOp::Equal => Some(Self::Equal),
            FilterOp::NotEqual => Some(Self::NotEqual),
            FilterOp::GreaterThan => Some(Self::GreaterThan),
            FilterOp::GreaterEqual => Some(Self::GreaterEqual),
            FilterOp::LessThan => Some(Self::LessThan),
            FilterOp::LessEqual => Some(Self::LessEqual),
            FilterOp::Like | FilterOp::In | FilterOp::IsNull | FilterOp::IsNotNull => None,
        }
    }

    /// The [`FilterOp`] this operator is the column form of.
    #[must_use]
    pub fn as_filter_op(self) -> FilterOp {
        match self {
            Self::Equal => FilterOp::Equal,
            Self::NotEqual => FilterOp::NotEqual,
            Self::GreaterThan => FilterOp::GreaterThan,
            Self::GreaterEqual => FilterOp::GreaterEqual,
            Self::LessThan => FilterOp::LessThan,
            Self::LessEqual => FilterOp::LessEqual,
        }
    }
}

/// A predicate tree for WHERE-clause construction: a leaf [`Filter`], a
/// column-to-column [`ColumnFilter`] leaf, or an `AND`/`OR` group of
/// sub-trees. This is the builder-input analogue of the
/// wire `FilterNode`; the database handler converts wire → this before
/// calling [`wafer_sql_utils`] builders, so the SQL layer never sees wire
/// types.
#[derive(Debug, Clone)]
pub enum FilterTree {
    /// A single comparison predicate.
    Leaf(Filter),
    /// A comparison between two columns of the same row.
    ColumnCompare(ColumnFilter),
    /// AND of child predicates.
    All(Vec<FilterTree>),
    /// OR of child predicates.
    Any(Vec<FilterTree>),
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

    #[test]
    fn column_compare_op_covers_exactly_the_comparison_operators() {
        let with_column_form = [
            FilterOp::Equal,
            FilterOp::NotEqual,
            FilterOp::GreaterThan,
            FilterOp::GreaterEqual,
            FilterOp::LessThan,
            FilterOp::LessEqual,
        ];
        for op in with_column_form {
            let column_op = ColumnCompareOp::from_filter_op(&op)
                .unwrap_or_else(|| panic!("{op:?} has a column form"));
            assert_eq!(column_op.as_filter_op(), op);
        }
        for op in [
            FilterOp::Like,
            FilterOp::In,
            FilterOp::IsNull,
            FilterOp::IsNotNull,
        ] {
            assert_eq!(ColumnCompareOp::from_filter_op(&op), None, "{op:?}");
        }
    }
}
