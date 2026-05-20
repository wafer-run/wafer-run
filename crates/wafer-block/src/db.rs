//! Database query types shared between `wafer-core` (client and service
//! layers) and `wafer-sql-utils` (query builders).
//!
//! Keeping these here breaks the circular dependency that would otherwise
//! arise if `wafer-sql-utils` depended on `wafer-core` for `Filter` etc.
//! while `wafer-core` needed `wafer-sql-utils` for the `Statement` type.

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
